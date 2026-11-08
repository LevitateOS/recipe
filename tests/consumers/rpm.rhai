const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ archive: "file" }, outputs: #{ rootfs: "directory" } };
let ctx = #{name: "rpm", state: #{installed: false}};
fn is_acquired(ctx) { verify_sha256("/inputs/archive", "1bab05fecdd71fc4a946852d83405f238c89cccb63987c0941c7f2f0184a99e2"); ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) { if ctx.state.installed { ready(ctx) } else { needs_work(ctx, "convert and extract pinned RPM") } }
fn install(ctx) {
    mkdir("/out/rootfs");
    exec_checked("bash", ["--noprofile", "--norc", "-o", "pipefail", "-c", "rpm2cpio /inputs/archive | cpio -idmu --quiet"], #{cwd: "/out/rootfs"});
    verify_sha256("/out/rootfs/usr/lib64/libz.so.1.3.1.zlib-ng", "bcc97bb783dd4986a54482af2182841712728646ed19236aac3f4aad6605e172");
    ctx.outputs.rootfs = "/out/rootfs"; ctx.state.installed = true; ctx
}
