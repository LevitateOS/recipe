use super::*;

#[test]
fn test_mkdir_recursive() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "mkdir-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test recursive directory creation
    mkdir(`${BUILD_DIR}/deep/nested/path/here`);

    if !dir_exists(`${BUILD_DIR}/deep/nested/path/here`) {
        throw "mkdir recursive failed";
    }
    if !dir_exists(`${BUILD_DIR}/deep/nested/path`) {
        throw "mkdir did not create parent";
    }
    if !dir_exists(`${BUILD_DIR}/deep/nested`) {
        throw "mkdir did not create grandparent";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("mkdir-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "mkdir recursive test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_glob_list_helper() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "glob-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Create test files
    mkdir(`${BUILD_DIR}/globtest`);
    shell(`echo "a" > ${BUILD_DIR}/globtest/file1.txt`);
    shell(`echo "b" > ${BUILD_DIR}/globtest/file2.txt`);
    shell(`echo "c" > ${BUILD_DIR}/globtest/file3.log`);

    // Test glob_list for .txt files
    let txt_files = glob_list(`${BUILD_DIR}/globtest/*.txt`);
    if txt_files.len() != 2 {
        throw `glob_list *.txt expected 2 files, got ${txt_files.len()}`;
    }

    // Test glob_list for .log files
    let log_files = glob_list(`${BUILD_DIR}/globtest/*.log`);
    if log_files.len() != 1 {
        throw `glob_list *.log expected 1 file, got ${log_files.len()}`;
    }

    // Test glob_list for all files
    let all_files = glob_list(`${BUILD_DIR}/globtest/*`);
    if all_files.len() != 3 {
        throw `glob_list * expected 3 files, got ${all_files.len()}`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("glob-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "glob_list test failed: {:?}", result.err());
}

#[test]
fn test_mv_and_ln_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "mv-ln-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    mkdir(`${BUILD_DIR}/mvtest`);

    // Create and move a file
    shell(`echo "content" > ${BUILD_DIR}/mvtest/original.txt`);
    mv(`${BUILD_DIR}/mvtest/original.txt`, `${BUILD_DIR}/mvtest/moved.txt`);

    if file_exists(`${BUILD_DIR}/mvtest/original.txt`) {
        throw "mv did not remove source";
    }
    if !file_exists(`${BUILD_DIR}/mvtest/moved.txt`) {
        throw "mv did not create dest";
    }

    // Create a symlink
    ln(`${BUILD_DIR}/mvtest/moved.txt`, `${BUILD_DIR}/mvtest/link.txt`);
    if !file_exists(`${BUILD_DIR}/mvtest/link.txt`) {
        throw "ln did not create symlink";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("mv-ln-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "mv/ln test failed: {:?}", result.err());
}

#[test]
fn test_shell_output_and_status() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "shell-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test shell_output
    let output = shell_output("echo hello");
    if !output.contains("hello") {
        throw `shell_output expected "hello", got "${output}"`;
    }

    // Test shell_status for success
    let status = shell_status("true");
    if status != 0 {
        throw `shell_status for 'true' expected 0, got ${status}`;
    }

    // Test shell_status for failure
    let fail_status = shell_status("false");
    if fail_status == 0 {
        throw `shell_status for 'false' expected non-zero, got ${fail_status}`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("shell-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "shell_output/shell_status test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_exec_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "exec-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test exec with args
    exec("echo", ["hello", "world"]);

    // Test exec_output
    let output = exec_output("echo", ["test", "args"]);
    if !output.contains("test") || !output.contains("args") {
        throw `exec_output expected "test args", got "${output}"`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("exec-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "exec/exec_output test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_env_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "env-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Set an environment variable
    set_env("RECIPE_TEST_VAR", "test_value");

    // Read it back
    let val = env("RECIPE_TEST_VAR");
    if val != "test_value" {
        throw `env expected "test_value", got "${val}"`;
    }

    // Test reading PATH (should exist)
    let path = env("PATH");
    if path.len() == 0 {
        throw "PATH environment variable is empty";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("env-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "env/set_env test failed: {:?}",
        result.err()
    );
}
