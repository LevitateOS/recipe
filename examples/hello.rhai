const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ program: "file" } };
const PACKAGE = #{ name: "hello", version: "1.0", description: "A one-line shell program" };

// `out` is where the program's file must be made.
fn build(src, out) {
    write_file(out, "#!/bin/sh\nprintf 'hello\\n'\n");
    chmod(out, 0o755);
}
