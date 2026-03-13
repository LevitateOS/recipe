#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
recipe_dir="$(cd -- "${script_dir}/.." && pwd)"
repo_root="$(cd -- "${recipe_dir}/../.." && pwd)"
cheat_test_dir="${repo_root}/testing/cheat-test"

usage() {
  cat <<'EOF'
Usage: tools/recipe/scripts/podman-test.sh <alpine|fedora|rocky|all>

Runs the full tools/recipe cargo test suite in a distro-native Podman container.

Targets:
  alpine  Alpine 3.23
  fedora  Fedora latest
  rocky   Rocky Linux 9
  all     Run all three sequentially
EOF
}

require_paths() {
  test -f "${recipe_dir}/Cargo.toml"
  test -f "${cheat_test_dir}/Cargo.toml"
}

run_alpine() {
  podman run --rm \
    -v "${recipe_dir}:/workspace:z" \
    -v "${cheat_test_dir}:/testing/cheat-test:z" \
    -w /workspace \
    alpine:3.23 \
    sh -lc '
      set -eux
      test -f Cargo.toml
      test -f /testing/cheat-test/Cargo.toml
      apk add --no-cache \
        curl ca-certificates gcc g++ musl-dev make pkgconf openssl-dev \
        bzip2-dev bzip2-static xz-dev xz-static zstd-dev git bash perl doas
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
      . "$HOME/.cargo/env"
      CARGO_TARGET_DIR=/tmp/recipe-target cargo test -- --nocapture
    '
}

run_fedora() {
  podman run --rm \
    -v "${recipe_dir}:/workspace:z" \
    -v "${cheat_test_dir}:/testing/cheat-test:z" \
    -w /workspace \
    fedora:latest \
    sh -lc '
      set -eux
      test -f Cargo.toml
      test -f /testing/cheat-test/Cargo.toml
      dnf install -y --setopt=install_weak_deps=False \
        gcc gcc-c++ make pkgconf-pkg-config openssl-devel \
        bzip2-devel xz-devel libzstd-devel which findutils tar gzip git
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
      . "$HOME/.cargo/env"
      CARGO_TARGET_DIR=/tmp/recipe-target cargo test -- --nocapture
    '
}

run_rocky() {
  podman run --rm \
    -v "${recipe_dir}:/workspace:z" \
    -v "${cheat_test_dir}:/testing/cheat-test:z" \
    -w /workspace \
    rockylinux:9 \
    sh -lc '
      set -eux
      test -f Cargo.toml
      test -f /testing/cheat-test/Cargo.toml
      dnf install -y epel-release
      dnf install -y --setopt=install_weak_deps=False \
        gcc gcc-c++ make pkgconf-pkg-config openssl-devel \
        bzip2-devel xz-devel libzstd-devel which findutils tar gzip git
      curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
      . "$HOME/.cargo/env"
      CARGO_TARGET_DIR=/tmp/recipe-target cargo test -- --nocapture
    '
}

main() {
  require_paths

  case "${1:-}" in
    alpine)
      run_alpine
      ;;
    fedora)
      run_fedora
      ;;
    rocky)
      run_rocky
      ;;
    all)
      run_alpine
      run_fedora
      run_rocky
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
}

main "$@"
