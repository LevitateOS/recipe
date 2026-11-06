const RECIPE_API = 2;
let ctx = #{
    name: "hello",
    version: "1.0",
    program: join_path(RUNTIME.paths.output, "bin/hello"),
    payload: "#!/bin/sh\nprintf 'hello\\n'\n",
};

fn is_acquired(ctx) { ready(ctx) }
fn acquire(ctx) { ctx }
fn is_built(ctx) { ready(ctx) }
fn build(ctx) { ctx }

fn is_installed(ctx) {
    if !is_file(ctx.program) { return needs_work(ctx, "hello output is missing"); }
    if read_file(ctx.program) != ctx.payload {
        return needs_work(ctx, "hello output has different contents");
    }
    ctx.outputs.program = ctx.program;
    ready(ctx)
}

fn install(ctx) {
    mkdir(dirname(ctx.program));
    write_file(ctx.program, ctx.payload);
    chmod(ctx.program, 0o755);
    ctx.outputs.program = ctx.program;
    ctx
}

fn remove(ctx) {
    if is_file(ctx.program) { rm(ctx.program); }
    ctx.outputs = #{};
    ctx
}
