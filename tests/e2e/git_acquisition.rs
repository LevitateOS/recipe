use super::*;

fn fixture_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Recipe CLI fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn upstream(root: &Path) -> (std::path::PathBuf, String, String) {
    let repo = root.join("upstream.git");
    std::fs::create_dir(&repo).unwrap();
    fixture_git(&repo, &["init", "--quiet"]);
    let first = commit_payload(&repo, "first source payload");
    let second = commit_payload(&repo, "second source payload");
    (repo, first, second)
}

fn commit_payload(repo: &Path, payload: &str) -> String {
    std::fs::write(repo.join("payload.txt"), payload).unwrap();
    fixture_git(repo, &["add", "payload.txt"]);
    fixture_git(repo, &["commit", "--quiet", "-m", payload]);
    fixture_git(repo, &["rev-parse", "HEAD"])
}

fn bare_upstream(root: &Path) -> (std::path::PathBuf, String, String) {
    let (source, first, second) = upstream(root);
    let bare = root.join("source.git");
    let output = Command::new("git")
        .args(["clone", "--bare", "--no-hardlinks"])
        .arg(&source)
        .arg(&bare)
        .output()
        .unwrap();
    assert_success(&output);
    (bare, first, second)
}

fn git_command(recipes: &Path, source: Option<&Path>) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([
            "install",
            "git-payload",
            "--no-persist-state",
            "--recipes-path",
        ])
        .arg(recipes);
    if let Some(source) = source {
        command
            .arg("--input")
            .arg(format!("source.git={}", source.display()));
    }
    command
}

#[test]
fn managed_pinned_git_selects_exact_commits_and_preserves_invalid_checkouts() {
    for failure in ["dirty", "origin", "invalid", "config"] {
        let (dir, recipes) = create_test_env();
        let (bare, first, second) = bare_upstream(dir.path());
        write_recipe(
            &recipes,
            "git-payload",
            &format!(
                r##"
const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ "source.git": "directory" }}, outputs: #{{ product: "file" }} }};
let ctx = #{{name: "git-payload", source: join_path(RUNTIME.paths.build, "exact checkout 'quoted'"),
    origin: "file:///inputs/source.git", state: #{{acquired: false, installed: false}}}};
fn is_acquired(ctx) {{
    if ctx.state.acquired {{ return ready(ctx); }}
    needs_work(ctx, "acquire exact commit")
}}
fn is_built(ctx) {{ needs_work(ctx, "source preparation required") }}
fn is_installed(ctx) {{
    if ctx.state.installed {{ return ready(ctx); }}
    needs_work(ctx, "install pinned source")
}}
fn acquire(ctx) {{
    ctx.source = git_checkout(ctx.origin, ctx.source, {first:?});
    if read_file(join_path(ctx.source, "payload.txt")) != "first source payload" {{ throw "wrong first commit"; }}
    // Cached exact commit reuse and changing selection use the same owner.
    git_checkout(ctx.origin, ctx.source, {first:?});
    git_checkout(ctx.origin, ctx.source, {second:?});
    if read_file(join_path(ctx.source, "payload.txt")) != "second source payload" {{ throw "wrong second commit"; }}
    if {failure:?} == "dirty" {{ write_file(join_path(ctx.source, "payload.txt"), "local edits"); }}
    else if {failure:?} == "origin" {{
        exec_checked("git", ["-C", ctx.source, "config", "remote.origin.url", "file:///inputs/wrong.git"]);
    }} else if {failure:?} == "config" {{
        let hooks = join_path(ctx.source, ".git/policy-hooks");
        mkdir(hooks);
        let hook = join_path(hooks, "post-checkout");
        write_file(hook, "#!/bin/sh\nprintf unsafe > /work/build/POLICY_EXECUTED\n");
        chmod(hook, 0o755);
        let monitor = join_path(ctx.source, ".git/policy-monitor");
        write_file(monitor, "#!/bin/sh\nprintf unsafe > /work/build/POLICY_EXECUTED\n");
        chmod(monitor, 0o755);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "core.hooksPath", hooks]);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "core.fsmonitor", monitor]);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "http.https://example.invalid/.sslVerify", "false"]);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "http.https://example.invalid/.proxy", "http://127.0.0.1:9"]);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "url.ext::unexpected.insteadOf", ctx.origin]);
        exec_checked("git", ["-C", ctx.source, "config", "--local", "include.path", "/work/build/extra.config"]);
        write_file("/work/build/extra.config", "[http]\nsslVerify = false\n");
    }} else {{ write_file(join_path(ctx.source, ".git/HEAD"), "not a Git HEAD"); }}
    let before = read_file(join_path(ctx.source, "payload.txt"));
    let rejected = false;
    try {{ git_checkout(ctx.origin, ctx.source, {first:?}); }} catch (error) {{ rejected = true; }}
    if !rejected {{ throw "invalid checkout was accepted"; }}
    if is_file("/work/build/POLICY_EXECUTED") {{ throw "cached Git policy executed a hook or monitor"; }}
    if read_file(join_path(ctx.source, "payload.txt")) != before {{ throw "checkout bytes were replaced"; }}
    if glob_list(join_path(dirname(ctx.source), ".recipe-git.*")).len != 0 {{ throw "Git staging leaked"; }}
    ctx.state.acquired = true;
    ctx
}}
fn install(ctx) {{
    copy_file(join_path(ctx.source, "payload.txt"), "/out/product");
    ctx.outputs.product = "/out/product";
    ctx.state.installed = true;
    ctx
}}
"##
            ),
        );
        let before = std::fs::read(recipes.join("git-payload.rhai")).unwrap();
        let output = git_command(&recipes, Some(&bare)).output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["state"]["installed"], true);
        let product = Path::new(context["outputs"]["product"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(product).unwrap(),
            if failure == "dirty" {
                "local edits"
            } else {
                "second source payload"
            }
        );
        assert_eq!(
            std::fs::read(recipes.join("git-payload.rhai")).unwrap(),
            before
        );
    }
}

#[test]
fn managed_failed_git_acquisition_keeps_staging_private() {
    let (dir, recipes) = create_test_env();
    let (bare, _, _) = bare_upstream(dir.path());
    write_recipe(
        &recipes,
        "git-payload",
        &format!(
            r##"
const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ "source.git": "directory" }} }};
let ctx = #{{name: "git-payload", source: join_path(RUNTIME.paths.build, "checkout")}};
fn is_acquired(ctx) {{ needs_work(ctx, "acquire missing commit") }}
fn is_built(ctx) {{ needs_work(ctx, "source preparation required") }}
fn is_installed(ctx) {{ needs_work(ctx, "install requested") }}
fn acquire(ctx) {{ git_checkout("file:///inputs/source.git", ctx.source, {missing:?}); ctx }}
fn cleanup(ctx, reason) {{
    if reason == "auto.acquire.failure" {{
        if is_dir(ctx.source) {{ throw "failed Git checkout was published"; }}
        if glob_list(join_path(dirname(ctx.source), ".recipe-git.*")).len != 0 {{ throw "Git staging leaked"; }}
        log("GIT_STAGING_PRESERVED");
    }}
    ctx
}}
"##,
            missing = "0".repeat(40)
        ),
    );
    let output = git_command(&recipes, Some(&bare)).output().unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("GIT_STAGING_PRESERVED"), "{diagnostic}");
}

#[test]
fn managed_git_rejects_unpinned_revisions_and_unsupported_origins_before_spawn() {
    for (origin, commit, expected) in [
        (
            "https://example.invalid/source.git",
            "main".to_owned(),
            "full 40- or 64-character",
        ),
        (
            "ssh://example.invalid/source.git",
            "0".repeat(40),
            "Git origin must use",
        ),
    ] {
        let (_dir, recipes) = create_test_env();
        write_recipe(
            &recipes,
            "git-payload",
            &format!(
                r##"
const RECIPE_API = 2;
const NEEDS = #{{}};
let ctx = #{{name: "git-payload"}};
fn is_acquired(ctx) {{ needs_work(ctx, "validate source") }}
fn is_built(ctx) {{ needs_work(ctx, "source preparation required") }}
fn is_installed(ctx) {{ needs_work(ctx, "install requested") }}
fn acquire(ctx) {{ git_checkout({origin:?}, "/work/build/checkout", {commit:?}); ctx }}
fn cleanup(ctx, reason) {{
    if is_dir("/work/build/checkout") {{ throw "invalid arguments created a checkout"; }}
    log("GIT_INVALID_INPUT_PRESERVED"); ctx
}}
"##
            ),
        );
        let output = git_command(&recipes, None).output().unwrap();
        assert!(!output.status.success());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains(expected), "{diagnostic}");
        assert!(
            diagnostic.contains("GIT_INVALID_INPUT_PRESERVED"),
            "{diagnostic}"
        );
    }
}

#[test]
fn managed_stalled_git_deadline_reaps_transport_and_releases_destination() {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let (dir, recipes) = create_test_env();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let source = format!(
        r#"
const RECIPE_API = 2;
const NEEDS = #{{ network: [{endpoint:?}], outputs: #{{ product: "file" }} }};
let ctx = #{{name: "git-stall"}};
fn is_acquired(ctx) {{ needs_work(ctx, "acquire stalled Git origin") }}
fn is_built(ctx) {{ needs_work(ctx, "acquire source before build") }}
fn is_installed(ctx) {{ needs_work(ctx, "install requested") }}
fn acquire(ctx) {{
    print("GIT_STALL_STARTED");
    git_checkout("http://{endpoint}/source.git", "/work/build/checkout", "0000000000000000000000000000000000000000");
    ctx
}}
fn install(ctx) {{ throw "stalled acquisition reached install"; }}
"#
    );
    write_recipe(&recipes, "git-payload", &source);
    let destination = dir.path().join("destination");
    let retained = dir.path().join("retained");
    let invoke = || {
        let mut command = recipe_command(&recipes);
        command
            .args(["install", "git-payload", "--recipes-path"])
            .arg(&recipes)
            .arg("--build-dir")
            .arg(&destination)
            .arg("--state-dir")
            .arg(&retained)
            .args(["--timeout-secs", "15"]);
        command
    };
    let stderr_path = dir.path().join("stalled-git.stderr");
    let started = Instant::now();
    let mut child = invoke()
        .stdout(Stdio::from(
            std::fs::File::create(dir.path().join("stalled-git.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
        .spawn()
        .unwrap();
    let (stream, _) = loop {
        match listener.accept() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child.try_wait().unwrap().is_some()
                    || started.elapsed() > Duration::from_secs(120)
                {
                    child.kill().ok();
                    child.wait().ok();
                    panic!(
                        "Git did not reach the silent HTTP origin: {}",
                        std::fs::read_to_string(&stderr_path).unwrap()
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("HTTP fixture: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request).unwrap();
    assert!(
        request.starts_with("GET /source.git/info/refs?"),
        "{request}"
    );
    // No response is sent. The real Git transport remains blocked, below the
    // helper's 600-second HTTP low-speed bound, until the managed deadline.
    fn descendants(pid: u32, seen: &mut Vec<(u32, String)>) {
        for task in std::fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .flatten()
        {
            for child in std::fs::read_to_string(task.path().join("children"))
                .unwrap_or_default()
                .split_whitespace()
            {
                let child: u32 = child.parse().unwrap();
                if seen.iter().any(|(pid, _)| *pid == child) {
                    continue;
                }
                if let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) {
                    let fields: Vec<_> = stat
                        .rsplit_once(") ")
                        .unwrap()
                        .1
                        .split_whitespace()
                        .collect();
                    seen.push((child, fields[19].to_owned()));
                    descendants(child, seen);
                }
            }
        }
    }
    let mut tracked = Vec::new();
    descendants(child.id(), &mut tracked);
    let commands: Vec<_> = tracked
        .iter()
        .filter_map(|(pid, _)| std::fs::read(format!("/proc/{pid}/cmdline")).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .collect();
    assert!(
        commands.iter().any(|s| s.contains("git-remote-http")),
        "actual Git transport grandchild absent: {commands:?}"
    );
    std::fs::write(
        dir.path().join("git-transport-processes.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"request": request, "processes": tracked, "commands": commands}),
        )
        .unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("managed Git deadline did not terminate coordinator");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stream);
    assert!(!status.success());
    assert!(started.elapsed() < Duration::from_secs(150));
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(
        stderr.contains("supervised execution deadline expired"),
        "{stderr}"
    );
    let reaping_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let live: Vec<_> = tracked
            .iter()
            .filter(|(pid, token)| {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .is_some_and(|stat| {
                        let fields: Vec<_> = stat
                            .rsplit_once(") ")
                            .unwrap()
                            .1
                            .split_whitespace()
                            .collect();
                        fields[19] == token.as_str() && fields[0] != "Z"
                    })
            })
            .collect();
        if live.is_empty() {
            break;
        }
        assert!(
            Instant::now() < reaping_deadline,
            "Git descendants survived deadline: {live:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let logs: Vec<_> = std::fs::read_dir(retained.join("logs"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().is_dir())
        .collect();
    assert_eq!(logs.len(), 1);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(logs[0].path().join("result.json")).unwrap())
            .unwrap();
    assert_eq!(record["status"], "failed");
    assert!(
        std::fs::read_to_string(logs[0].path().join("worker.log"))
            .unwrap()
            .contains("GIT_STALL_STARTED")
    );
    assert!(!dir.path().join(".artifacts/ginux-export").exists());
    assert_eq!(
        std::fs::read_to_string(recipes.join("git-payload.rhai")).unwrap(),
        source
    );
    // A new source identity uses the same destination lock immediately, and
    // succeeds without importing the killed acquisition's private staging.
    write_recipe(
        &recipes,
        "git-payload",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ product: "file" } };
let ctx = #{name: "git-stall", state: #{installed: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) { if ctx.state.installed { ctx.outputs.product="/out/product"; return ready(ctx); } needs_work(ctx,"install") }
fn install(ctx) {
    if is_dir("/work/build/checkout") || glob_list("/work/build/.recipe-git.*").len != 0 { throw "stalled Git staging crossed source identities"; }
    write_file("/out/product","recovered after stalled Git");
    ctx.outputs.product="/out/product"; ctx.state.installed=true; ctx
}
"#,
    );
    let recovered = invoke().output().unwrap();
    assert_success(&recovered);
    let result: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "recovered after stalled Git"
    );
}
