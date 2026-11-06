#!/usr/bin/env bash
# Privileged behavioral check matching UNTANGLE.md ## Baseline.
# Usage: bash .untangle/priv-test.sh <out-dir>   (run from tools/recipe)
set -u
OUT=${1:?out dir}; mkdir -p "$OUT"
ROOT=$(cd ../.. && pwd)
cd "$ROOT"
cargo build --offline -p levitate-recipe --bin recipe >"$OUT/build.log" 2>&1 || { echo BUILD_FAILED; exit 2; }
E=$(cargo test --offline -p levitate-recipe --test e2e --no-run --message-format=json 2>>"$OUT/build.log" | grep -o '"executable":"[^"]*e2e-[^"]*"' | cut -d'"' -f4)
L=$(cargo test --offline -p levitate-recipe --test library_backend --no-run --message-format=json 2>>"$OUT/build.log" | grep -o '"executable":"[^"]*library_backend-[^"]*"' | cut -d'"' -f4)
I=$(cargo test --offline -p levitate-recipe --test image_producer --no-run --message-format=json 2>>"$OUT/build.log" | grep -o '"executable":"[^"]*image_producer-[^"]*"' | cut -d'"' -f4)
cp target/debug/recipe "$OUT/recipe-bin"; cp "$E" "$OUT/e2e"; cp "$L" "$OUT/libbackend"; cp "$I" "$OUT/imageproducer"
cp /tmp/ginux-t24-hidden-devices "$OUT/ginux"
ENVS="RECIPE_TEST_BIN=$OUT/recipe-bin RECIPE_WORKER_BIN=$OUT/recipe-bin GINUX_BIN=$OUT/ginux GINUX_ROOTFS=$ROOT/.artifacts/ginux-env/prepared-image-git-musl/rootfs.tar GINUX_ROOT=oci:recipe-compiler@sha256:9f220633d34efff68b1535375fbb4fa3c4c2c8843917859fd68d8f6edec27ff4"
"$OUT/imageproducer" >"$OUT/image.log" 2>&1; echo "EXIT=$?" >>"$OUT/image.log"
sudo -n env $ENVS RECIPE_WORKER=1 RECIPE_INVOCATION=spoofed-public-call "$OUT/libbackend" >"$OUT/lib.log" 2>&1; echo "EXIT=$?" >>"$OUT/lib.log"
sudo -n env $ENVS "$OUT/e2e" --test-threads=6 >"$OUT/e2e.log" 2>&1; echo "EXIT=$?" >>"$OUT/e2e.log"
grep -h "^test result" "$OUT/image.log" "$OUT/lib.log" "$OUT/e2e.log"
grep -h "FAILED" "$OUT/e2e.log" | head -40
