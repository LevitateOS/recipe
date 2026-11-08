const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ program: "file" }, memory_mib: 4096, timeout_secs: 600 };
let ctx = #{name:"threaded-fresh-host", state:#{builds:0,installs:0}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if is_file("/work/build/cargo-target/debug/threaded-hello") { return ready(ctx); } needs_work(ctx,"compile program") }
fn build(ctx) {
 if ctx.state.builds != 0 { throw "successful build was unexpectedly repeated"; }
 mkdir("/work/build/project/src");
 write_file("/work/build/project/Cargo.toml", "[package]\nname=\"threaded-hello\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[workspace]\n");
 write_file("/work/build/project/src/main.rs", "fn main(){let hs:Vec<_>=(0..6).map(|_|std::thread::spawn(||7u32)).collect();let n:u32=hs.into_iter().map(|h|h.join().unwrap()).sum();assert_eq!(n,42);println!(\"threaded-hello:{n}\");}\n");
 exec_checked("cargo", ["build","--offline","--manifest-path","/work/build/project/Cargo.toml","--target-dir","/work/build/cargo-target","--jobs","1"]);
 exec_checked("/work/build/cargo-target/debug/threaded-hello", []);
 ctx.state.builds += 1; ctx
}
fn is_installed(ctx) { if is_file("/out/threaded-hello") && ctx.state.installs == 1 { ctx.outputs.program="/out/threaded-hello"; return ready(ctx); } needs_work(ctx,"install program") }
fn install(ctx) { exec_checked("cp",["/work/build/cargo-target/debug/threaded-hello","/out/threaded-hello"]); ctx.outputs.program="/out/threaded-hello"; ctx.state.installs += 1; ctx }
