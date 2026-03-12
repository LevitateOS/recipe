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
fn test_glob_exists_and_copy_into_dir() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "glob-copy-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    let src = `${BUILD_DIR}/copy-src`;
    let dst = `${BUILD_DIR}/copy-dst`;
    mkdir(src);
    mkdir(dst);

    write_file(join_path(src, "a.txt"), "alpha");
    write_file(join_path(src, "b.txt"), "beta");
    write_file(join_path(src, "skip.log"), "log");

    if !glob_exists(join_path(src, "*.txt")) {
        throw "glob_exists did not find txt files";
    }
    if glob_exists(join_path(src, "*.rpm")) {
        throw "glob_exists reported false positive";
    }

    copy_into_dir(join_path(src, "*.txt"), dst);
    if !is_file(join_path(dst, "a.txt")) || !is_file(join_path(dst, "b.txt")) {
        throw "copy_into_dir did not copy expected files";
    }
    if is_file(join_path(dst, "skip.log")) {
        throw "copy_into_dir copied an unexpected file";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("glob-copy-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "glob_exists/copy_into_dir test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_copy_helpers_and_append_line_if_missing() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "copy-helpers-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    let src = `${BUILD_DIR}/tree-src`;
    let dst = `${BUILD_DIR}/tree-dst`;
    mkdir(join_path(src, "nested"));
    mkdir(dst);

    write_file(join_path(src, "top.txt"), "alpha");
    write_file(join_path(src, "nested/child.txt"), "beta");

    let copied = `${BUILD_DIR}/exact-copy.txt`;
    copy_file(join_path(src, "top.txt"), copied);
    if read_file(copied) != "alpha" {
        throw "copy_file did not preserve content";
    }

    let reflink_copy = `${BUILD_DIR}/reflink-copy.txt`;
    copy_file_reflink(join_path(src, "top.txt"), reflink_copy);
    if read_file(reflink_copy) != "alpha" {
        throw "copy_file_reflink did not preserve content";
    }

    copy_tree_contents(src, dst);
    if read_file(join_path(dst, "top.txt")) != "alpha" {
        throw "copy_tree_contents missed top-level file";
    }
    if read_file(join_path(dst, "nested/child.txt")) != "beta" {
        throw "copy_tree_contents missed nested file";
    }

    let selected = copy_first_existing(
        [
            join_path(src, "missing.txt"),
            join_path(src, "nested/child.txt"),
            join_path(src, "top.txt"),
        ],
        `${BUILD_DIR}/selected.txt`
    );
    if selected != join_path(src, "nested/child.txt") {
        throw `copy_first_existing chose wrong source: ${selected}`;
    }
    if read_file(`${BUILD_DIR}/selected.txt`) != "beta" {
        throw "copy_first_existing did not copy selected file";
    }

    let lines = `${BUILD_DIR}/lines.txt`;
    write_file(lines, "alpha\n");
    if append_line_if_missing(lines, "alpha") {
        throw "append_line_if_missing reported change for existing line";
    }
    if !append_line_if_missing(lines, "beta") {
        throw "append_line_if_missing did not report append";
    }
    if append_line_if_missing(lines, "beta") {
        throw "append_line_if_missing appended duplicate line";
    }

    let final_lines = read_file(lines);
    if final_lines != "alpha\nbeta\n" {
        throw `append_line_if_missing produced wrong content: ${final_lines}`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("copy-helpers-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "copy helper test failed: {:?}",
        result.err()
    );
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
fn test_ln_force_and_replace_in_file() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "ln-force-replace-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    let src = `${BUILD_DIR}/ln-src`;
    mkdir(src);
    write_file(join_path(src, "one.txt"), "one");
    write_file(join_path(src, "two.txt"), "two");

    let link = `${BUILD_DIR}/current.txt`;
    ln(join_path(src, "one.txt"), link);
    ln_force(join_path(src, "two.txt"), link);

    let resolved = shell_output(`readlink ${link}`);
    if trim(resolved) != join_path(src, "two.txt") {
        throw `ln_force did not replace link target: ${resolved}`;
    }

    let desktop = `${BUILD_DIR}/sample.desktop`;
    write_file(desktop, "Exec=kitty\nTryExec=kitty\n");
    replace_in_file(desktop, "Exec=kitty", "Exec=/tmp/kitty");
    replace_in_file(desktop, "TryExec=kitty", "TryExec=/tmp/kitty");
    let updated = read_file(desktop);
    if !contains(updated, "Exec=/tmp/kitty") || !contains(updated, "TryExec=/tmp/kitty") {
        throw "replace_in_file did not rewrite desktop file";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("ln-force-replace-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "ln_force/replace_in_file test failed: {:?}",
        result.err()
    );
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
fn test_package_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r##"
let ctx = #{
    name: "package-helpers-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    let bin_dir = `${BUILD_DIR}/fake-bin`;
    let log_file = `${BUILD_DIR}/dnf.log`;
    let downloads_dir = `${BUILD_DIR}/downloads`;
    mkdir(bin_dir);
    mkdir(downloads_dir);

    write_file(join_path(bin_dir, "sudo"), "#!/bin/sh\nif [ \"$1\" = \"-n\" ]; then shift; fi\nexec \"$@\"\n");
    write_file(
        join_path(bin_dir, "rpm"),
        "#!/bin/sh\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"fakepkg\" ]; then exit 0; fi\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"missingpkg\" ]; then exit 1; fi\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"--qf\" ] && [ \"$4\" = \"fakepkg\" ]; then printf '1.2.3'; exit 0; fi\nexit 1\n"
    );
    write_file(
        join_path(bin_dir, "dnf"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"" + log_file + "\"\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"info\" ] && [ \"$3\" = \"fakepkg\" ]; then exit 0; fi\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"info\" ] && [ \"$3\" = \"missingpkg\" ]; then exit 1; fi\nif [ \"$1\" = \"config-manager\" ] && [ \"$2\" = \"--add-repo\" ]; then exit 0; fi\nif [ \"$1\" = \"install\" ] && [ \"$2\" = \"-y\" ]; then exit 0; fi\nif [ \"$1\" = \"download\" ]; then\n  destdir=\"\"\n  while [ $# -gt 0 ]; do\n    case \"$1\" in\n      --destdir=*) destdir=${1#--destdir=} ;;\n    esac\n    shift\n  done\n  : \"${destdir:?missing destdir}\"\n  touch \"$destdir/alpha-1.0.noarch.rpm\" \"$destdir/beta-2.0.x86_64.rpm\"\n  exit 0\nfi\nexit 1\n"
    );
    shell(`chmod +x ${bin_dir}/sudo ${bin_dir}/rpm ${bin_dir}/dnf`);
    set_env("PATH", bin_dir + ":" + env("PATH"));

    if !rpm_installed("fakepkg") { throw "rpm_installed false negative"; }
    if rpm_installed("missingpkg") { throw "rpm_installed false positive"; }
    if rpm_version("fakepkg") != "1.2.3" { throw "rpm_version wrong"; }
    if !dnf_package_available("fakepkg") { throw "dnf_package_available false negative"; }
    if dnf_package_available("missingpkg") { throw "dnf_package_available false positive"; }

    dnf_add_repo("https://example.invalid/repo");
    dnf_install(["alpha", "beta"]);
    dnf_install_allow_erasing(["gamma"]);
    let downloaded = dnf_download(["alpha", "beta"], downloads_dir, ["x86_64", "noarch"]);
    if downloaded.len() != 2 {
        throw `dnf_download expected 2 files, got ${downloaded.len()}`;
    }
    let downloaded_no_resolve = dnf_download(["alpha"], downloads_dir, ["x86_64"], false);
    if downloaded_no_resolve.len() != 0 {
        throw `dnf_download second pass expected 0 new files, got ${downloaded_no_resolve.len()}`;
    }

    let log = read_file(log_file);
    if !contains(log, "config-manager --add-repo https://example.invalid/repo") {
        throw "dnf_add_repo did not invoke config-manager";
    }
    if !contains(log, "install -y alpha beta") {
        throw "dnf_install did not pass packages";
    }
    if !contains(log, "install -y --allowerasing gamma") {
        throw "dnf_install_allow_erasing missing flag";
    }
    if !contains(log, "download -q --resolve") {
        throw "dnf_download missing resolve call";
    }
    if !contains(log, "--destdir=" + downloads_dir) {
        throw "dnf_download missing destination directory";
    }
    if !contains(log, "--arch x86_64 --arch noarch alpha beta") {
        throw "dnf_download missing arch/package args";
    }
    if !contains(log, "download -q --destdir=" + downloads_dir + " --arch x86_64 alpha") {
        throw "dnf_download(false) missing non-resolve call";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"##;

    let recipe_path = recipes_dir.join("package-helpers-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "package helper test failed: {:?}",
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
