#!/usr/bin/env python3
"""Real CLI consumers; requires the documented configured-launch privileges.

No host compilation or workload fallback. Each invocation writes a receipt even
on failure. Use a fresh output directory to preserve earlier failed evidence.
"""
import argparse
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import subprocess
import threading
import time


HERE = Path(__file__).resolve().parent


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def checked(args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def render(template, values):
    text = (HERE / template).read_text()
    for name, value in values.items():
        text = text.replace("@" + name + "@", json.dumps(value))
    require(not re.search(r"@[A-Z]+@", text), "unexpanded recipe placeholder")
    return text


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, fmt, *args):
        with (self.server.evidence / "http.log").open("a") as log:
            log.write(fmt % args + "\n")


def git_http(work):
    source = work / "source"
    source.mkdir()
    checked(["git", "init", "--quiet", str(source)])
    (source / "payload").write_text("exact HTTP source\n")
    checked(["git", "-C", str(source), "add", "payload"])
    checked(["git", "-C", str(source), "-c", "user.name=Recipe consumer", "-c",
             "user.email=consumer@example.invalid", "commit", "--quiet", "-m", "fixture"],
            env={**os.environ, "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
                 "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"})
    commit = checked(["git", "-C", str(source), "rev-parse", "HEAD"])
    checked(["git", "clone", "--bare", "--no-hardlinks", str(source), str(work / "source.git")], stderr=subprocess.STDOUT)
    checked(["git", "-C", str(work / "source.git"), "update-server-info"])
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(work)))
    server.evidence = work
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = "127.0.0.1:" + str(server.server_port)
    values = dict(ENDPOINT=endpoint, URL="http://" + endpoint + "/source.git",
                  COMMIT=commit, PAYLOAD="payload", DIGEST=digest(source / "payload"))
    return server, values


def apk_recipe(lock):
    text = lock.read_text()
    rows = []
    for name in ["apk-tools-static", "musl", "busybox"]:
        row = re.search(r'\["' + name + r'",[^\n]+\]', text)
        require(row is not None, "missing locked APK " + name)
        rows.append(json.loads(row[0]))
    key = re.search(r'\["alpine-devel[^\n]+\]', text)
    require(key is not None, "missing Alpine signing key")
    key = json.loads(key[0])
    return render("apk.rhai.in", dict(ROWS=rows, TOOLNAME=rows[0][2].rsplit("/", 1)[1],
                  PACKAGES=[r[2].rsplit("/", 1)[1] for r in rows[1:]], KEYNAME=key[0], KEY=key[1])), rows


def package_payload_digest(packages, name):
    import gzip
    import io
    import tarfile
    for path in packages.glob("*.apk"):
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(path.read_bytes())), ignore_zeros=True) as archive:
            for member in archive:
                if member.name.lstrip("./") == name:
                    return hashlib.sha256(archive.extractfile(member).read()).hexdigest()
    raise RuntimeError("pinned package payload missing: " + name)


def validate(kind, ctx):
    outputs = {k: Path(v) for k, v in ctx["outputs"].items()}
    for name in ["program"] if kind in ("sqlite", "apk") else (["gnu", "musl"] if kind == "cargo" else []):
        require(outputs[name].read_bytes().startswith(b"\x7fELF"), "not an ELF: " + name)
        require(stat.S_IMODE(outputs[name].stat().st_mode) == 0o755, "wrong executable mode: " + name)
    if kind == "sqlite":
        require(ctx["state"]["result"] == "sqlite-locks: PASS\n", "SQLite contention failed")
        # Read-only inspection of the published data, never a host workload substitute.
        with sqlite3.connect(outputs["database"].as_uri() + "?mode=ro", uri=True) as db:
            require(db.execute("SELECT sum(v), count(*) FROM items").fetchone() == (33, 2), "published data differs")
            require(db.execute("PRAGMA integrity_check").fetchone() == ("ok",), "published DB is corrupt")
    elif kind == "cargo":
        logs = [(outputs["logs"] / (t + "-" + c + ".log")).read_text()
                for t in ("gnu", "musl") for c in ("first", "second")]
        require(all("Finished" in log for log in logs), "a Cargo client failed")
        require(sum(log.count("Compiling consumer v0.1.0") for log in logs) == 2, "expected exactly one compilation per target")
        require(any("Blocking waiting for file lock" in log for log in logs), "no actual lock contention observed")
        require((outputs["cache"] / ".package-cache").is_file(), "shared package cache lock missing")
        for target in ("gnu", "musl"):
            require(ctx["state"][target] == "compiled rust 42\n", "threaded program failed")
        headers = checked(["readelf", "-lW", str(outputs["musl"])])
        require("INTERP" not in headers, "musl product is not static")
    elif kind == "rpm":
        root = outputs["rootfs"]
        library = root / "usr/lib64/libz.so.1.3.1.zlib-ng"
        require(digest(library) == "bcc97bb783dd4986a54482af2182841712728646ed19236aac3f4aad6605e172", "RPM payload differs")
        require(library.stat().st_size == 169536 and stat.S_IMODE(library.stat().st_mode) == 0o755, "RPM size/mode differs")
        require(library.stat().st_uid == 0 and library.stat().st_gid == 0, "RPM ownership differs")
        require(os.readlink(root / "usr/lib64/libz.so.1") == "libz.so.1.3.1.zlib-ng", "RPM symlink differs")
        require((root / "usr/lib/.build-id/f1/616a2a6c431967ce6600fdbb462f917d445430").resolve() == library, "RPM build-ID link differs")
    elif kind == "apk":
        require(ctx["state"]["uid"] == ctx["state"]["gid"] == "1000", "APK ran as root")
        require(ctx["state"]["busybox"] == "installed APK executed\n", "installed APK failed")
        metadata = json.loads((outputs["rootfs"] / ".recipe-target-metadata.json").read_text())
        busybox = metadata["entries"]["bin/busybox"]
        require(busybox["uid"] == busybox["gid"] == 0 and busybox["mode"] == 0o755, "APK intended metadata differs")
        require(digest(outputs["rootfs"] / "bin/busybox") == package_payload_digest(outputs["packages"], "bin/busybox"), "APK installed bytes differ from pinned package")
    return {k: {"path": str(v), "sha256": digest(v)} if v.is_file() else {"path": str(v)} for k, v in outputs.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=["sqlite", "cargo", "git-http", "git-https", "rpm", "apk"])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rpm", type=Path)
    parser.add_argument("--apk-lock", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    work = args.output.resolve()
    recipe = Path(os.environ["RECIPE_TEST_BIN"]).resolve()
    image = Path(os.environ["GINUX_ROOTFS"]).resolve()
    identity = {name: {"path": str(path), "sha256": digest(path)} for name, path in
                [("recipe", recipe), ("worker", Path(os.environ["RECIPE_WORKER_BIN"])),
                 ("ginux", Path(os.environ["GINUX_BIN"])), ("image", image)]}
    require(os.environ["GINUX_ROOT"].endswith("sha256:" + identity["image"]["sha256"]), "image reference mismatch")
    receipt = dict(case=args.case, identities=identity, status="preparing", image_reference=os.environ["GINUX_ROOT"],
                   runner_sha256=digest(__file__), fixture_files={p.name: digest(p) for p in HERE.iterdir() if p.is_file()})
    server = None
    command = [str(recipe), "install", "consumer", "--recipes-path", str(work / "recipes"),
               "--build-dir", str(work / "publication"), "--state-dir", str(work / "state"), "--jobs", "1", "--no-persist-state"]
    started = time.monotonic()
    try:
        if args.case == "git-http":
            server, values = git_http(work)
            content = render("git.rhai.in", values)
            receipt["git"] = values
        elif args.case == "git-https":
            values = dict(ENDPOINT="github.com:443", URL="https://github.com/LevitateOS/recipe.git",
                          COMMIT="43d9dd4e1296799467f42b2c026aa107c3022d8d", PAYLOAD="README.md",
                          DIGEST="18d54af802f6db610cb5fa06f3442087be470e44fc2671889ac5723a0ee2c946")
            content = render("git.rhai.in", values)
            receipt["git"] = values
        elif args.case == "apk":
            require(args.apk_lock is not None, "--apk-lock required")
            content, rows = apk_recipe(args.apk_lock)
            receipt["apk_lock"] = dict(path=str(args.apk_lock.resolve()), sha256=digest(args.apk_lock), rows=rows)
        else:
            content = (HERE / (args.case + ".rhai")).read_text()
        if args.case == "sqlite":
            command += ["--input", "source=" + str(HERE / "sqlite.c")]
        if args.case == "rpm":
            require(args.rpm is not None, "--rpm required")
            command += ["--input", "archive=" + str(args.rpm.resolve())]
            receipt["rpm"] = dict(path=str(args.rpm.resolve()), sha256=digest(args.rpm))
        (work / "recipes").mkdir()
        (work / "recipes/consumer.rhai").write_text(content)
        receipt.update(command=command, recipe_sha256=digest(work / "recipes/consumer.rhai"), status="running")
        (work / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        env = {**os.environ, **{f"XDG_{k}_HOME": str(work / k.lower()) for k in ("DATA", "STATE", "CONFIG", "CACHE")}, "TMPDIR": str(work)}
        with (work / "stdout.json").open("w") as stdout, (work / "stderr.log").open("w") as stderr:
            completed = subprocess.run(["timeout", "--signal=INT", "--kill-after=15s", "480s", *command],
                                       cwd=work, env=env, stdout=stdout, stderr=stderr)
        receipt["exit_code"] = completed.returncode
        require(completed.returncode == 0, "Recipe failed; see stderr.log")
        ctx = json.loads((work / "stdout.json").read_text())
        receipt["products"] = validate(args.case, ctx)
        if args.case.startswith("git-"):
            require(digest(Path(ctx["outputs"]["payload"])) == values["DIGEST"], "published Git payload differs")
        if args.case == "apk":
            for row in rows:
                require(digest(Path(ctx["outputs"]["packages"]) / row[2].rsplit("/", 1)[1]) == row[3], "published APK digest differs")
        receipt["status"] = "passed"
    except Exception as error:
        receipt.update(status="failed", error=str(error))
    finally:
        if server:
            server.shutdown()
            server.server_close()
        receipt["elapsed_seconds"] = round(time.monotonic() - started, 3)
        receipt["logs"] = {p.name: digest(p) for p in [work / "stdout.json", work / "stderr.log"] if p.is_file()}
        (work / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps({k: receipt.get(k) for k in ("case", "status", "exit_code", "elapsed_seconds", "error")}))
    raise SystemExit(0 if receipt["status"] == "passed" else 1)


if __name__ == "__main__":
    main()
