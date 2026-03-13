#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
recipe_dir="$(cd -- "${script_dir}/.." && pwd)"
repo_root="$(cd -- "${recipe_dir}/../.." && pwd)"
cheat_test_dir="${repo_root}/testing/cheat-test"

version="$(
  sed -n 's/^version = "\(.*\)"/\1/p' "${recipe_dir}/Cargo.toml" | head -n 1
)"

if [[ -z "${version}" ]]; then
  echo "failed to read version from ${recipe_dir}/Cargo.toml" >&2
  exit 1
fi

podman run --rm \
  -v "${recipe_dir}:/workspace:z" \
  -v "${cheat_test_dir}:/testing/cheat-test:z" \
  -w /workspace \
  alpine:3.23 \
  sh -lc "
    set -eux
    test -f Cargo.toml
    test -f /testing/cheat-test/Cargo.toml
    apk add --no-cache \
      curl ca-certificates gcc g++ musl-dev make pkgconf openssl-dev \
      bzip2-dev bzip2-static xz-dev xz-static zstd-dev tar gzip
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    . \"\$HOME/.cargo/env\"
    CARGO_TARGET_DIR=/tmp/recipe-target cargo build --release
    arch=\$(apk --print-arch)
    pkgroot=/tmp/pkgroot
    outdir=/workspace/target/packages/apk
    mkdir -p \"\$pkgroot\" \"\$outdir\"
    install -Dm755 /tmp/recipe-target/release/recipe \"\$pkgroot/usr/bin/recipe\"
    install -Dm644 README.md \"\$pkgroot/usr/share/doc/levitate-recipe/README.md\"
    install -Dm644 docs/man/recipe.1 \"\$pkgroot/usr/share/man/man1/recipe.1\"
    install -Dm644 docs/man/recipe-recipe.5 \"\$pkgroot/usr/share/man/man5/recipe-recipe.5\"
    install -Dm644 docs/man/recipe-helpers.7 \"\$pkgroot/usr/share/man/man7/recipe-helpers.7\"
    install -Dm644 LICENSE-MIT \"\$pkgroot/usr/share/licenses/levitate-recipe/LICENSE-MIT\"
    install -Dm644 LICENSE-APACHE \"\$pkgroot/usr/share/licenses/levitate-recipe/LICENSE-APACHE\"
    apk mkpkg \
      --files \"\$pkgroot\" \
      --info name:levitate-recipe \
      --info version:${version}-r0 \
      --info description:Rhai-based package recipe executor for LevitateOS \
      --info arch:\$arch \
      --info license:MIT\ OR\ Apache-2.0 \
      --info origin:levitate-recipe \
      --info url:https://github.com/levitate-os/levitate-recipe \
      --output \"\$outdir/levitate-recipe-${version}-r0.\$arch.apk\"
  "

echo "built target/packages/apk/levitate-recipe-${version}-r0.x86_64.apk"
