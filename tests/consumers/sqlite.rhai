const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ source: "file" }, outputs: #{ database: "file", program: "file" } };
fn build(src, out) {
    let program = join_path(WORK, "sqlite-consumer");
    exec_checked("gcc", ["-O2", "-Wall", "-Wextra", "-Werror", "-x", "c", INPUTS.source, "-x", "none", "/usr/lib64/libsqlite3.so.0", "-o", program]);
    if exec_output(program, []) != "sqlite-locks: PASS\n" { throw "SQLite transaction fixture failed"; }
    copy_file("/work/cache.db", out.database);
    copy_file(program, out.program);
}
