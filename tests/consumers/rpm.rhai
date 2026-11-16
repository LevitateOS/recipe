const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ archive: "file" }, outputs: #{ rootfs: "directory" } };
fn build(src, out) {
    verify_sha256(INPUTS.archive, "1bab05fecdd71fc4a946852d83405f238c89cccb63987c0941c7f2f0184a99e2");
    exec_checked("bash", ["--noprofile", "--norc", "-o", "pipefail", "-c", "rpm2cpio " + INPUTS.archive + " | cpio -idmu --quiet"], #{cwd: out});
    verify_sha256(join_path(out, "usr/lib64/libz.so.1.3.1.zlib-ng"), "bcc97bb783dd4986a54482af2182841712728646ed19236aac3f4aad6605e172");
}
