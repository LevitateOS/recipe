#!/usr/bin/env python3
"""Real CLI/Ginux ripgrep install, update, failed update and offline removal.

Run the whole script with the native launcher's bootstrap privileges and the
same GINUX_BIN/GINUX_ROOTFS/GINUX_ROOT selections as ordinary Recipe execution.
All installed files, state, publications and logs stay below --artifacts.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--recipe", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    source = args.recipe.resolve(strict=True).read_text()
    root = args.artifacts.resolve()
    root.mkdir(parents=True, exist_ok=False)
    catalog = root / "catalog"
    catalog.mkdir()
    recipe = catalog / "ripgrep.rhai"
    package_root = root / "packages"
    env = dict(os.environ, RECIPE_WORKER_BIN=str(binary))
    base = [str(binary), "--recipes-path", str(catalog), "--package-root",
            str(package_root), "--state-dir", str(root / "state"),
            "--build-dir", str(root / "build"), "--json"]

    def invoke(label, *command, success=True, offline=False):
        selected_env = env.copy()
        if offline:
            selected_env.update(GINUX_BIN="/missing-ginux", GINUX_ROOTFS="/missing-rootfs")
        with (root / f"{label}.json").open("w") as out, (root / f"{label}.log").open("w") as err:
            result = subprocess.run(base + list(command), cwd=root, env=selected_env,
                                    stdout=out, stderr=err, timeout=600)
        if (result.returncode == 0) != success:
            raise RuntimeError(f"{label}: unexpected exit {result.returncode}; see {root / (label + '.log')}")
        print(f"{label}: expected exit {result.returncode}", flush=True)
        return json.loads((root / f"{label}.json").read_text()) if success else None

    def version(expected):
        rg = package_root / "profile/bin/rg"
        text = subprocess.check_output([str(rg), "--version"], text=True)
        if text.splitlines()[0].split()[:2] != ["ripgrep", expected]:
            raise RuntimeError(f"active ripgrep version differs: {text}")
        fixture = root / "search.txt"
        fixture.write_text("first\nrecipe-activation-needle\nlast\n")
        found = subprocess.check_output([str(rg), "recipe-activation-needle", str(fixture)], text=True)
        if found != "recipe-activation-needle\n":
            raise RuntimeError("active rg failed the real search")

    # Both archives and payload hashes were verified against upstream releases.
    # Keep the earlier version here to exercise an actual executable replacement.
    previous = source
    for current, old in [
        ('version: "15.2.0"', 'version: "15.1.0"'),
        ("33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
         "1c9297be4a084eea7ecaedf93eb03d058d6faae29bbc57ecdaf5063921491599"),
        ("e62198eb19b136b88c330af83647b5a962cb99b6b1f066758568f12de1974849",
         "ebeaf56f8a25e102e9419933423738b3a2a613a444fd749d695e15eba53f71f2"),
        ("2b0db3d436bc1b79c2da556bb75ef1797c00476208c1fe04e0f6e012690912bf",
         "f2097dadba0668217de5af6728b929fb78f82e2bbe4b0c3392244f78da4bddc1"),
    ]:
        if previous.count(current) != 1:
            raise RuntimeError("ripgrep pin changed; refresh the verified update fixture")
        previous = previous.replace(current, old)
    recipe.write_text(previous)
    invoke("install-15.1.0", "install", "ripgrep")
    version("15.1.0")
    invoke("isinstalled", "isinstalled", "ripgrep", offline=True)
    before = os.readlink(package_root / "profile")

    recipe.write_text(source)
    invoke("update-15.2.0", "update", "ripgrep")
    version("15.2.0")
    if os.readlink(package_root / "profile") == before:
        raise RuntimeError("update did not switch the active generation")
    current_profile = os.readlink(package_root / "profile")
    installed = invoke("installed", "installed", offline=True)
    if [(p["name"], p["version"]) for p in installed["packages"]] != [("ripgrep", "15.2.0")]:
        raise RuntimeError("installed receipt does not match active ripgrep")

    # A new input identity with denied acquisition must preserve active software.
    recipe.write_text(source.replace('version: "15.2.0"', 'version: "15.2.0-unavailable"'))
    invoke("failed-update", "update", "ripgrep", "--no-network", success=False)
    if os.readlink(package_root / "profile") != current_profile:
        raise RuntimeError("failed update changed the active profile")
    version("15.2.0")

    # Removal uses the installed receipt even after catalog/runtime disappearance.
    recipe.unlink()
    invoke("remove-without-runtime", "remove", "ripgrep", offline=True)
    if (package_root / "profile/bin/rg").exists():
        raise RuntimeError("removed rg remains active")
    remaining = invoke("installed-empty", "installed", offline=True)
    if remaining["packages"]:
        raise RuntimeError("removed package remains in active receipt")
    invoke("isinstalled-after-remove", "isinstalled", "ripgrep", offline=True, success=False)
    (root / "verified.json").write_text(json.dumps({
        "install": "15.1.0", "update": "15.2.0", "real_search": True,
        "failed_update_preserves_profile": True, "removal_without_catalog_or_runtime": True,
    }, indent=2) + "\n")
    print(f"Verified install/update/remove: {root / 'verified.json'}", flush=True)


if __name__ == "__main__":
    main()
