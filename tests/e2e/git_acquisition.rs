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
    for failure in ["dirty", "origin", "invalid"] {
        let (dir, recipes) = create_test_env();
        let (bare, first, second) = bare_upstream(dir.path());
        write_recipe(
            &recipes,
            "git-payload",
            &format!(
                r#"
const RECIPE_API = 2;
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
    }} else {{ write_file(join_path(ctx.source, ".git/HEAD"), "not a Git HEAD"); }}
    let before = read_file(join_path(ctx.source, "payload.txt"));
    let rejected = false;
    try {{ git_checkout(ctx.origin, ctx.source, {first:?}); }} catch (error) {{ rejected = true; }}
    if !rejected {{ throw "invalid checkout was accepted"; }}
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
"#
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
        assert_eq!(fixture_git(&bare, &["rev-parse", "HEAD"]), second);
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
            r#"
const RECIPE_API = 2;
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
"#,
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
                r#"
const RECIPE_API = 2;
let ctx = #{{name: "git-payload"}};
fn is_acquired(ctx) {{ needs_work(ctx, "validate source") }}
fn is_built(ctx) {{ needs_work(ctx, "source preparation required") }}
fn is_installed(ctx) {{ needs_work(ctx, "install requested") }}
fn acquire(ctx) {{ git_checkout({origin:?}, "/work/build/checkout", {commit:?}); ctx }}
fn cleanup(ctx, reason) {{
    if is_dir("/work/build/checkout") {{ throw "invalid arguments created a checkout"; }}
    log("GIT_INVALID_INPUT_PRESERVED"); ctx
}}
"#
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
