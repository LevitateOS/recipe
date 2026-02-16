use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(unix)]
fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

fn init_git_repo(dir: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
}

fn write_llm_toml(cfg_home: &Path, codex_bin: &str, claude_bin: &str) {
    fs::create_dir_all(cfg_home.join("recipe")).unwrap();
    fs::write(
        cfg_home.join("recipe/llm.toml"),
        format!(
            r#"
version = 1
default_provider = "codex"
default_profile = "codex"
timeout_secs = 5
max_output_bytes = 1048576
max_input_bytes = 1048576

[providers.codex]
bin = "{codex_bin}"
args = []

[providers.claude]
bin = "{claude_bin}"
args = ["-p","--output-format","text","--no-chrome"]

[profiles.codex]
default_provider = "codex"

[profiles.codex.providers.codex]
model = "gpt-5.1-codex-mini"
"#,
            codex_bin = codex_bin,
            claude_bin = claude_bin
        ),
    )
    .unwrap();
}

fn write_stub_codex(bin_dir: &Path, patch: &str) -> PathBuf {
    let stub_codex = bin_dir.join("codex");
    fs::write(
        &stub_codex,
        format!(
            r#"#!/bin/sh
need_model="gpt-5.1-codex-mini"
out=""
model=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--model" ]; then
    model="$2"
    shift 2
    continue
  fi
  if [ "$1" = "--output-last-message" ]; then
    out="$2"
    shift 2
    continue
  fi
  shift
done
cat >/dev/null
if [ -z "$out" ]; then
  echo "missing --output-last-message" 1>&2
  exit 2
fi
if [ "$model" != "$need_model" ]; then
  echo "wrong model: got '$model', want '$need_model'" 1>&2
  exit 3
fi
cat >"$out" <<'EOF'
{patch}
EOF
exit 0
"#,
            patch = patch
        ),
    )
    .unwrap();
    chmod_x(&stub_codex);
    stub_codex
}

fn write_stub_claude(bin_dir: &Path) -> PathBuf {
    let stub_claude = bin_dir.join("claude");
    fs::write(
        &stub_claude,
        r#"#!/bin/sh
cat >/dev/null
echo "claude-result"
exit 0
"#,
    )
    .unwrap();
    chmod_x(&stub_claude);
    stub_claude
}

fn broken_recipe() -> &'static str {
    r#"let ctx = #{
    name: "autofix-test",
    installed: false,
    marker: join_path(BUILD_DIR, "marker"),
};

fn is_acquired(ctx) { ctx }
fn is_built(ctx) { if !is_file(ctx.marker) { throw "not built"; } ctx }
fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }

fn acquire(ctx) { ctx }
fn build(ctx) {
    shell("false");
    ctx
}
fn install(ctx) {
    ctx.installed = true;
    ctx
}
fn cleanup(ctx, reason) { ctx }
"#
}

fn broken_recipe_multiline_checks() -> &'static str {
    r#"let ctx = #{
    name: "autofix-test",
    installed: false,
    marker: join_path(BUILD_DIR, "marker"),
};

fn is_acquired(ctx) { ctx }
fn is_built(ctx) {
    if !is_file(ctx.marker) { throw "not built"; }
    ctx
}
fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn build(ctx) {
    shell("false");
    ctx
}
fn install(ctx) {
    ctx.installed = true;
    ctx
}
fn cleanup(ctx, reason) { ctx }
"#
}

fn fix_build_patch() -> &'static str {
    r#"diff --git a/autofix-test.rhai b/autofix-test.rhai
--- a/autofix-test.rhai
+++ b/autofix-test.rhai
@@ -12,4 +12,4 @@ fn acquire(ctx) { ctx }
 fn build(ctx) {
-    shell("false");
+    write_file(ctx.marker, "ok");
     ctx
 }
"#
}

fn reward_hack_patch() -> &'static str {
    r#"diff --git a/autofix-test.rhai b/autofix-test.rhai
--- a/autofix-test.rhai
+++ b/autofix-test.rhai
@@ -9,1 +9,1 @@ fn is_built(ctx) { if !is_file(ctx.marker) { throw "not built"; } ctx }
-fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }
+fn is_installed(ctx) { ctx }
"#
}

fn reward_hack_body_patch() -> &'static str {
    r#"diff --git a/autofix-test.rhai b/autofix-test.rhai
--- a/autofix-test.rhai
+++ b/autofix-test.rhai
@@ -12,4 +12,4 @@
 fn is_installed(ctx) {
-    if !ctx.installed { throw "not installed"; }
+    // reward hack: always claim installed
     ctx
 }
"#
}

#[test]
#[cfg(unix)]
fn autofix_retries_and_succeeds() {
    let _guard = test_lock().lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_home = tmp.path().join("cfg");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let stub_codex = write_stub_codex(&bin_dir, fix_build_patch());
    let stub_claude = write_stub_claude(&bin_dir);
    write_llm_toml(
        &cfg_home,
        &stub_codex.display().to_string(),
        &stub_claude.display().to_string(),
    );

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        std::env::set_var("XDG_CONFIG_DIRS", "");
    }

    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    let recipe_path = repo.path().join("autofix-test.rhai");
    fs::write(&recipe_path, broken_recipe()).unwrap();

    let build_dir = tempfile::TempDir::new().unwrap();
    let engine = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
        .with_llm_profile(Some("codex".to_owned()))
        .with_autofix(Some(levitate_recipe::AutoFixConfig {
            attempts: 1,
            cwd: Some(repo.path().to_path_buf()),
            prompt_file: None,
            allow_paths: Vec::new(),
        }));

    let _ctx = engine
        .execute(&recipe_path)
        .expect("install should succeed after autofix");

    let updated = fs::read_to_string(&recipe_path).unwrap();
    assert!(
        updated.contains("write_file(ctx.marker"),
        "recipe not patched:\n{updated}"
    );
    assert!(
        updated.contains("installed: true"),
        "ctx not persisted:\n{updated}"
    );
}

#[test]
#[cfg(unix)]
fn autofix_rejects_reward_hacks() {
    let _guard = test_lock().lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_home = tmp.path().join("cfg");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let stub_codex = write_stub_codex(&bin_dir, reward_hack_patch());
    let stub_claude = write_stub_claude(&bin_dir);
    write_llm_toml(
        &cfg_home,
        &stub_codex.display().to_string(),
        &stub_claude.display().to_string(),
    );

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        std::env::set_var("XDG_CONFIG_DIRS", "");
    }

    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    let recipe_path = repo.path().join("autofix-test.rhai");
    fs::write(&recipe_path, broken_recipe()).unwrap();

    let build_dir = tempfile::TempDir::new().unwrap();
    let engine = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
        .with_llm_profile(Some("codex".to_owned()))
        .with_autofix(Some(levitate_recipe::AutoFixConfig {
            attempts: 1,
            cwd: Some(repo.path().to_path_buf()),
            prompt_file: None,
            allow_paths: Vec::new(),
        }));

    let err = engine.execute(&recipe_path).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("must not change lifecycle check functions"),
        "unexpected error:\n{msg}"
    );
}

#[test]
#[cfg(unix)]
fn autofix_rejects_lifecycle_body_edits() {
    let _guard = test_lock().lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_home = tmp.path().join("cfg");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let stub_codex = write_stub_codex(&bin_dir, reward_hack_body_patch());
    let stub_claude = write_stub_claude(&bin_dir);
    write_llm_toml(
        &cfg_home,
        &stub_codex.display().to_string(),
        &stub_claude.display().to_string(),
    );

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        std::env::set_var("XDG_CONFIG_DIRS", "");
    }

    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    let recipe_path = repo.path().join("autofix-test.rhai");
    fs::write(&recipe_path, broken_recipe_multiline_checks()).unwrap();

    let build_dir = tempfile::TempDir::new().unwrap();
    let engine = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
        .with_llm_profile(Some("codex".to_owned()))
        .with_autofix(Some(levitate_recipe::AutoFixConfig {
            attempts: 1,
            cwd: Some(repo.path().to_path_buf()),
            prompt_file: None,
            allow_paths: Vec::new(),
        }));

    let err = engine.execute(&recipe_path).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("modifies protected lifecycle function"),
        "unexpected error:\n{msg}"
    );
}

fn main_recipe_with_build_dep() -> &'static str {
    r#"let build_deps = ["dep"];

let ctx = #{
    name: "main",
    installed: false,
    marker: join_path(BUILD_DIR, "main-marker"),
};

fn is_acquired(ctx) { ctx }
fn is_built(ctx) { if !is_file(ctx.marker) { throw "not built"; } ctx }
fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }

fn acquire(ctx) { ctx }
fn build(ctx) { write_file(ctx.marker, "ok"); ctx }
fn install(ctx) { ctx.installed = true; ctx }
fn cleanup(ctx, reason) { ctx }
"#
}

fn broken_build_dep_recipe() -> &'static str {
    r#"let ctx = #{
    name: "dep",
    marker: join_path(TOOLS_PREFIX, "dep-ok"),
};

fn is_acquired(ctx) { ctx }
fn is_installed(ctx) {
    if !is_file(ctx.marker) { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn install(ctx) {
    mkdir(TOOLS_PREFIX);
    shell("false");
    ctx
}
fn cleanup(ctx, reason) { ctx }
"#
}

fn fix_build_dep_patch() -> &'static str {
    r#"diff --git a/dep.rhai b/dep.rhai
--- a/dep.rhai
+++ b/dep.rhai
@@ -13,5 +13,5 @@
 fn install(ctx) {
     mkdir(TOOLS_PREFIX);
-    shell("false");
+    write_file(ctx.marker, "ok");
     ctx
 }
"#
}

#[test]
#[cfg(unix)]
fn autofix_can_fix_build_deps() {
    let _guard = test_lock().lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_home = tmp.path().join("cfg");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let stub_codex = write_stub_codex(&bin_dir, fix_build_dep_patch());
    let stub_claude = write_stub_claude(&bin_dir);
    write_llm_toml(
        &cfg_home,
        &stub_codex.display().to_string(),
        &stub_claude.display().to_string(),
    );

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        std::env::set_var("XDG_CONFIG_DIRS", "");
    }

    let repo = tempfile::TempDir::new().unwrap();
    init_git_repo(repo.path());

    let dep_path = repo.path().join("dep.rhai");
    let main_path = repo.path().join("main.rhai");
    fs::write(&dep_path, broken_build_dep_recipe()).unwrap();
    fs::write(&main_path, main_recipe_with_build_dep()).unwrap();

    let build_dir = tempfile::TempDir::new().unwrap();
    let engine = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
        .with_recipes_path(repo.path().to_path_buf())
        .with_llm_profile(Some("codex".to_owned()))
        .with_autofix(Some(levitate_recipe::AutoFixConfig {
            attempts: 1,
            cwd: Some(repo.path().to_path_buf()),
            prompt_file: None,
            allow_paths: Vec::new(),
        }));

    let _ctx = engine
        .execute(&main_path)
        .expect("install should succeed after autofixing build-dep");

    let updated = fs::read_to_string(&dep_path).unwrap();
    assert!(
        updated.contains("write_file(ctx.marker"),
        "dep recipe not patched:\n{updated}"
    );
}
