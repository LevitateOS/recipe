const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ gnu: "file", musl: "file", logs: "directory", cache: "directory" }, timeout_secs: 420 };
let ctx = #{name: "cargo", state: #{built: false, installed: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if ctx.state.built { ready(ctx) } else { needs_work(ctx, "four concurrent Cargo clients") } }
fn is_installed(ctx) { if ctx.state.installed { ready(ctx) } else { needs_work(ctx, "publish threaded products") } }
fn build(ctx) {
    mkdir("/work/build/project/src");
    mkdir("/work/build/logs");
    write_file("/work/build/project/Cargo.toml", "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[profile.dev]\ndebug = 0\ncodegen-units = 1\nincremental = false\n");
    write_file("/work/build/project/src/main.rs", "fn main() { let n = std::thread::spawn(|| 6 * 7).join().unwrap(); println!(\"compiled rust {}\", n); }\n");
    exec_checked("bash", ["--noprofile", "--norc", "-c", "set -eu; pids=(); for target in gnu musl; do for client in first second; do cargo build --offline --jobs 1 --target x86_64-unknown-linux-$target --target-dir /work/build/cargo-target > /work/build/logs/$target-$client.log 2>&1 & pids+=($!); done; done; status=0; for pid in ${pids[@]}; do wait $pid || status=$?; done; cat /work/build/logs/*.log; exit $status"], #{cwd: "/work/build/project", env: #{CARGO_HOME: "/work/cargo-home", CARGO_NET_OFFLINE: "true", CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS: "-C linker=/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld -C linker-flavor=ld.lld"}});
    for target in ["gnu", "musl"] {
        let result = exec_output("/work/build/cargo-target/x86_64-unknown-linux-" + target + "/debug/consumer", []);
        if result != "compiled rust 42\n" { throw "threaded program failed: " + target; }
        ctx.state[target] = result;
    }
    ctx.state.built = true; ctx
}
fn install(ctx) {
    copy_file("/work/build/cargo-target/x86_64-unknown-linux-gnu/debug/consumer", "/out/gnu");
    copy_file("/work/build/cargo-target/x86_64-unknown-linux-musl/debug/consumer", "/out/musl");
    mkdir("/out/logs"); mkdir("/out/cache");
    copy_tree_contents("/work/build/logs", "/out/logs");
    copy_tree_contents("/work/cargo-home", "/out/cache");
    ctx.outputs = #{gnu: "/out/gnu", musl: "/out/musl", logs: "/out/logs", cache: "/out/cache"};
    ctx.state.installed = true; ctx
}
