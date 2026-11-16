const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ gnu: "file", musl: "file", logs: "directory", cache: "directory" }, timeout_secs: 420 };
fn build(src, out) {
    let project = join_path(WORK, "project");
    let logs = join_path(WORK, "logs");
    let cargo_target = join_path(WORK, "cargo-target");
    mkdir(join_path(project, "src"));
    mkdir(logs);
    write_file(join_path(project, "Cargo.toml"), "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[profile.dev]\ndebug = 0\ncodegen-units = 1\nincremental = false\n");
    write_file(join_path(project, "src/main.rs"), "fn main() { let n = std::thread::spawn(|| 6 * 7).join().unwrap(); println!(\"compiled rust {}\", n); }\n");
    exec_checked("bash", ["--noprofile", "--norc", "-c", "set -eu; pids=(); for target in gnu musl; do for client in first second; do cargo build --offline --jobs 1 --target x86_64-unknown-linux-$target --target-dir " + cargo_target + " > " + logs + "/$target-$client.log 2>&1 & pids+=($!); done; done; status=0; for pid in ${pids[@]}; do wait $pid || status=$?; done; cat " + logs + "/*.log; exit $status"], #{cwd: project, env: #{CARGO_HOME: "/work/cargo-home", CARGO_NET_OFFLINE: "true", CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS: "-C linker=/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld -C linker-flavor=ld.lld"}});
    for kind in ["gnu", "musl"] {
        let program = join_path(cargo_target, "x86_64-unknown-linux-" + kind + "/debug/consumer");
        if exec_output(program, []) != "compiled rust 42\n" { throw "threaded program failed: " + kind; }
    }
    copy_file(join_path(cargo_target, "x86_64-unknown-linux-gnu/debug/consumer"), out.gnu);
    copy_file(join_path(cargo_target, "x86_64-unknown-linux-musl/debug/consumer"), out.musl);
    copy_tree_contents(logs, out.logs);
    copy_tree_contents("/work/cargo-home", out.cache);
}
