const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ source: "file" }, outputs: #{ database: "file", program: "file" } };
let ctx = #{name: "sqlite", state: #{built: false, installed: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if ctx.state.built { ready(ctx) } else { needs_work(ctx, "compile and contend on SQLite") } }
fn is_installed(ctx) { if ctx.state.installed { ready(ctx) } else { needs_work(ctx, "publish checked database") } }
fn build(ctx) {
    exec_checked("gcc", ["-O2", "-Wall", "-Wextra", "-Werror", "-x", "c", "/inputs/source", "-x", "none", "/usr/lib64/libsqlite3.so.0", "-o", "/work/build/sqlite-consumer"]);
    ctx.state.result = exec_output("/work/build/sqlite-consumer", []);
    if ctx.state.result != "sqlite-locks: PASS\n" { throw "SQLite transaction fixture failed"; }
    ctx.state.built = true; ctx
}
fn install(ctx) {
    copy_file("/work/cache.db", "/out/cache.db");
    copy_file("/work/build/sqlite-consumer", "/out/sqlite-consumer");
    ctx.outputs = #{database: "/out/cache.db", program: "/out/sqlite-consumer"};
    ctx.state.installed = true; ctx
}
