#!/usr/bin/env python3
"""Real CLI checks for package activation boundaries using a verified rg product.

The selected rg executable must come from a previously verified upstream recipe
publication. Ginux imports it through a typed input; there is no host activation
shortcut. All generated recipes, outputs and stores stay under a new artifact
directory. Run with the configured launcher's whole-coordinator privileges.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


FIXTURE = '''const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ program: "file" }, outputs: #{ "package": "directory" }, timeout_secs: 120 };
let ctx = #{ name: @NAME@, version: @VERSION@,
    source: join_path(RUNTIME.paths.inputs, "program"),
    package_root: join_path(RUNTIME.paths.output, "package") };
fn is_acquired(ctx) { ready(ctx) }
fn acquire(ctx) { ctx }
fn is_built(ctx) { ready(ctx) }
fn build(ctx) { ctx }
fn is_installed(ctx) {
    if !is_file(join_path(ctx.package_root, "bin/rg")) { return needs_work(ctx, "copy rg"); }
    ctx.outputs["package"] = ctx.package_root;
    ready(ctx)
}
fn install(ctx) {
    mkdir(join_path(ctx.package_root, "bin"));
    copy_file(ctx.source, join_path(ctx.package_root, "bin/rg"));
    chmod(join_path(ctx.package_root, "bin/rg"), 0o755);
    ctx.outputs["package"] = ctx.package_root;
    ctx
}
'''


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--program", type=Path, required=True,
                        help="verified real rg executable from a Recipe publication")
    parser.add_argument("--artifacts", type=Path, required=True,
                        help="new evidence directory; existing directories are refused")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    program = args.program.resolve(strict=True)
    run = args.artifacts.absolute()
    for key in ("GINUX_BIN", "GINUX_ROOTFS", "GINUX_ROOT"):
        require(os.environ.get(key), f"{key} must select the configured runtime")
    env = {**os.environ, "RECIPE_WORKER_BIN": str(binary)}
    run.mkdir(parents=True, exist_ok=False)
    version_output = subprocess.check_output([str(program), "--version"], text=True)
    require(version_output.startswith("ripgrep "), "--program is not the verified rg executable")
    version = version_output.splitlines()[0].split()[1]
    for name in ("boundary-rg", "boundary-conflict"):
        (run / f"{name}.rhai").write_text(
            FIXTURE.replace("@NAME@", json.dumps(name)).replace("@VERSION@", json.dumps(version)))
    commands = []
    summary = {"binary_sha256": digest(binary), "program_sha256": digest(program),
               "program_version": version_output.splitlines()[0], "commands": commands}

    def install(label, name, store, extra=(), success=True):
        command = [str(binary), "install", str(run / f"{name}.rhai"),
                   "--input", f"program={program}", "--package-root", str(store),
                   "--state-dir", str(run / "state"), "--build-dir", str(run / f"work-{name}"),
                   "--json", *extra]
        result = subprocess.run(command, env=env, cwd=run, capture_output=True, text=True, timeout=600)
        (run / f"{label}.log").write_text(result.stderr)
        (run / f"{label}.json").write_text(result.stdout)
        commands.append({"label": label, "argv": command, "returncode": result.returncode})
        require((result.returncode == 0) == success,
                f"{label}: exit {result.returncode}: {result.stderr[-5000:]}")
        print(f"{label}: expected exit {result.returncode}", flush=True)
        return result

    try:
        store = run / "packages"
        install("01-produce-only", "boundary-rg", store, ("--no-activate",))
        require(not os.path.lexists(store / "profile"), "--no-activate created an active profile")
        summary["produce_only_no_activation"] = True
        install("02-activate", "boundary-rg", store)
        profile = store / "profile"
        active_before = profile.readlink()
        receipt_before = (profile / ".installed.json").read_bytes()
        require(digest(profile / "bin/rg") == digest(program), "active program differs from publication")
        require(subprocess.check_output([str(profile / "bin/rg"), "--version"], text=True)
                == version_output, "active rg reports the wrong version")
        conflict = install("03-conflict", "boundary-conflict", store, success=False)
        require("package file conflict" in conflict.stderr, "failure was not the expected command conflict")
        require(profile.readlink() == active_before, "conflict changed the active generation")
        require((profile / ".installed.json").read_bytes() == receipt_before,
                "conflict changed the active receipt")
        require(subprocess.check_output([str(profile / "bin/rg"), "--version"], text=True)
                == version_output, "conflict changed the active program")
        summary["conflict_preserved_active_rg"] = True
        unmanaged = run / "unmanaged"
        (unmanaged / "profile").mkdir(parents=True)
        sentinel = unmanaged / "profile/keep-me"
        sentinel.write_text("unrelated user data\n")
        failure = install("04-unmanaged-profile", "boundary-rg", unmanaged, success=False)
        require("unmanaged profile" in failure.stderr, "failure was not the unmanaged-profile check")
        require(sentinel.read_text() == "unrelated user data\n", "unrelated profile data changed")
        require(not (unmanaged / "profile").is_symlink(), "unmanaged directory was replaced")
        summary["unmanaged_profile_preserved"] = True
        summary["ok"] = True
    except Exception as error:
        summary["ok"] = False
        summary["error"] = str(error)
        raise
    finally:
        (run / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
