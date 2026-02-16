use std::fs;
use std::path::Path;
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

const AUTOFIX_FILENAME: &str = "llm-autofix-build-failure.rhai";
const PROVIDER_SMOKE_FILENAME: &str = "llm-provider-smoke.rhai";

fn broken_recipe_source() -> String {
    // Intentionally fake: the build phase throws. cleanup() uses llm_extract() to produce a full
    // fixed recipe and overwrites this file when the *real* engine reason is auto.build.failure.
    format!(
        r#"
let ctx = #{{ name: "llm-autofix-build-failure", installed: false, recipe_file: join_path(RECIPE_DIR, "{fname}") }};

fn acquire(ctx) {{
    // fake acquire
    ctx
}}

fn build(ctx) {{
    // purposeful error
    throw "INTENTIONAL_BUILD_ERROR";
}}

fn install(ctx) {{
    // should not run
    ctx.installed = true;
    ctx
}}

fn cleanup(ctx, reason) {{
    if reason == "auto.build.failure" || reason == "build.failed" {{
        let original = read_file(ctx.recipe_file);
        let fixed = llm_extract(
            original,
            "AUTOFIX_RECIPE: Fix the build error. Return the full corrected Rhai recipe source."
        );
        write_file(ctx.recipe_file, fixed);
    }}
    ctx
}}
"#,
        fname = AUTOFIX_FILENAME
    )
}

fn fixed_recipe_source() -> String {
    format!(
        r#"
// FIXED_BY_LLM
let ctx = #{{ name: "llm-autofix-build-failure", installed: false, recipe_file: join_path(RECIPE_DIR, "{fname}") }};

fn acquire(ctx) {{ ctx }}
fn build(ctx) {{ ctx.build_fixed = true; ctx }}
fn install(ctx) {{ ctx.installed = true; ctx }}
fn cleanup(ctx, reason) {{ ctx }}
"#,
        fname = AUTOFIX_FILENAME
    )
}

fn reset_recipe(path: &Path) {
    fs::write(path, broken_recipe_source()).unwrap();
}

fn provider_smoke_recipe_source() -> &'static str {
    r#"
let ctx = #{ name: "llm-provider-smoke", installed: false, r: "" };

fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }
fn is_built(ctx) { if ctx.r == "" { throw "not built"; } ctx }
fn is_acquired(ctx) { ctx }

fn acquire(ctx) { ctx }
fn build(ctx) {
    ctx.r = llm_extract("CONTENT", "PROMPT");
    ctx
}
fn install(ctx) { ctx.installed = true; ctx }

fn cleanup(ctx, reason) { ctx }
"#
}

#[test]
#[cfg(unix)]
fn llm_helpers_work_with_codex_and_claude() {
    let _guard = test_lock().lock().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_home = tmp.path().join("cfg");
    fs::create_dir_all(cfg_home.join("recipe")).unwrap();

    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let use_system_agents = std::env::var("RECIPE_TEST_SYSTEM_AGENTS")
        .ok()
        .is_some_and(|v| v.trim() == "1");

    let codex_bin: String;
    let claude_bin: String;

    if use_system_agents {
        codex_bin = "codex".to_owned();
        claude_bin = "claude".to_owned();
    } else {
        let stub_codex = bin_dir.join("codex");
        fs::write(
            &stub_codex,
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
input="$(cat)"
if [ -z "$out" ]; then
  echo "missing --output-last-message" 1>&2
  exit 2
fi
if [ "$model" != "$need_model" ]; then
  echo "wrong model: got '$model', want '$need_model'" 1>&2
  exit 3
fi
if echo "$input" | grep -q "AUTOFIX_RECIPE"; then
  cat >"$out" <<'EOF'
"#
            .to_owned()
                + fixed_recipe_source().as_str()
                + r#"
EOF
else
  echo "codex-result" > "$out"
fi
exit 0
"#,
        )
        .unwrap();
        chmod_x(&stub_codex);

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

        codex_bin = stub_codex.display().to_string();
        claude_bin = stub_claude.display().to_string();
    }

    let llm_toml = cfg_home.join("recipe/llm.toml");
    let codex_args = if use_system_agents {
        // These are `codex exec` flags (not top-level flags).
        r#"["--sandbox","read-only","--skip-git-repo-check"]"#
    } else {
        r#"[]"#
    };
    let timeout_secs = if use_system_agents { 120 } else { 5 };
    fs::write(
        &llm_toml,
        format!(
            r#"
version = 1
default_provider = "codex"
default_profile = "codex"
timeout_secs = {timeout_secs}
max_output_bytes = 1048576
max_input_bytes = 1048576

[providers.codex]
bin = "{codex_bin}"
args = {codex_args}

[providers.claude]
bin = "{claude_bin}"
args = ["-p","--output-format","text","--no-chrome"]

[profiles.codex]
default_provider = "codex"

[profiles.codex.providers.codex]
model = "gpt-5.1-codex-mini"

[profiles.claude]
default_provider = "claude"
"#,
            codex_bin = codex_bin,
            codex_args = codex_args,
            timeout_secs = timeout_secs,
            claude_bin = claude_bin
        ),
    )
    .unwrap();

    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        std::env::set_var("XDG_CONFIG_DIRS", "");
    }

    // Provider smoke: run the same recipe with two different LLM profiles.
    if !use_system_agents {
        let recipe_dir = tempfile::TempDir::new().unwrap();
        let recipe_path = recipe_dir.path().join(PROVIDER_SMOKE_FILENAME);
        fs::write(&recipe_path, provider_smoke_recipe_source()).unwrap();

        let build_dir = tempfile::TempDir::new().unwrap();

        let engine_codex = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
            .with_llm_profile(Some("codex".to_owned()));
        let ctx = engine_codex.execute(&recipe_path).unwrap();
        assert_eq!(
            ctx.get("r").and_then(|v| v.clone().into_string().ok()),
            Some("codex-result".to_owned())
        );

        // Reset for claude run.
        fs::write(&recipe_path, provider_smoke_recipe_source()).unwrap();
        let engine_claude = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
            .with_llm_profile(Some("claude".to_owned()));
        let ctx = engine_claude.execute(&recipe_path).unwrap();
        assert_eq!(
            ctx.get("r").and_then(|v| v.clone().into_string().ok()),
            Some("claude-result".to_owned())
        );
    }

    // Repeatable auto-fix scenario: reset broken recipe, run, verify rewritten to fixed.
    let recipe_dir = tempfile::TempDir::new().unwrap();
    let recipe_path = recipe_dir.path().join(AUTOFIX_FILENAME);

    let build_dir = tempfile::TempDir::new().unwrap();
    let engine = levitate_recipe::RecipeEngine::new(build_dir.path().to_path_buf())
        .with_llm_profile(Some("codex".to_owned()));

    for _ in 0..2 {
        reset_recipe(&recipe_path);
        let err = engine.execute(&recipe_path).unwrap_err();
        let fixed = fs::read_to_string(&recipe_path).unwrap();
        if use_system_agents {
            // We can't demand a specific marker from the real model output; just verify it rewrote
            // the recipe so the intentional throw is gone.
            let broken = broken_recipe_source();
            assert!(
                fixed != broken,
                "expected recipe to change after LLM cleanup; got unchanged recipe:\n{fixed}\nerror={err}"
            );
            assert!(
                !fixed.contains("INTENTIONAL_BUILD_ERROR"),
                "expected LLM to remove the intentional build error; got:\n{fixed}\nerror={err}"
            );
        } else {
            assert!(
                fixed.contains("FIXED_BY_LLM"),
                "expected recipe to be rewritten by LLM cleanup; got:\n{fixed}\nerror={err}"
            );
        }
    }
}
