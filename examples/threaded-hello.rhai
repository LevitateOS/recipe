const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ program: "file" }, memory_mib: 4096, timeout_secs: 600 };
const PACKAGE = #{ name: "threaded-hello", version: "0.1.0", description: "A compiled program that runs six threads" };

fn build(src, out) {
    let project = join_path(WORK, "project");
    let target = join_path(WORK, "cargo-target");
    mkdir(join_path(project, "src"));
    write_file(join_path(project, "Cargo.toml"), "[package]\nname=\"threaded-hello\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[workspace]\n");
    write_file(join_path(project, "src/main.rs"), "fn main(){let hs:Vec<_>=(0..6).map(|_|std::thread::spawn(||7u32)).collect();let n:u32=hs.into_iter().map(|h|h.join().unwrap()).sum();assert_eq!(n,42);println!(\"threaded-hello:{n}\");}\n");
    exec_checked("cargo", ["build", "--offline", "--manifest-path", join_path(project, "Cargo.toml"), "--target-dir", target, "--jobs", "1"]);
    let program = join_path(target, "debug/threaded-hello");
    exec_checked(program, []);
    copy_file(program, out);
}
