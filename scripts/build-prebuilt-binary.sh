#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

docker_arch_from_uname() {
  case "$(uname -m)" in
    x86_64|amd64) printf 'amd64' ;;
    aarch64|arm64) printf 'arm64' ;;
    *)
      printf 'unsupported host architecture: %s\n' "$(uname -m)" >&2
      exit 1
      ;;
  esac
}

docker_arch_from_rust_target() {
  case "$1" in
    x86_64-unknown-linux-gnu) printf 'amd64' ;;
    aarch64-unknown-linux-gnu) printf 'arm64' ;;
    *)
      printf 'unsupported Rust target for Docker prebuilt image: %s\n' "$1" >&2
      exit 1
      ;;
  esac
}

binary_name="mediaforge"
cargo_jobs="${MEDIAFORGE_CARGO_JOBS:-1}"
cargo_target_dir="${CARGO_TARGET_DIR:-target}"
cargo_args=(build --release --locked -j "$cargo_jobs" -p mediaforge-cli)

if [[ -n "${MEDIAFORGE_CARGO_TARGET:-}" ]]; then
  cargo_args+=(--target "$MEDIAFORGE_CARGO_TARGET")
  built_binary="$cargo_target_dir/$MEDIAFORGE_CARGO_TARGET/release/$binary_name"
  docker_arch="$(docker_arch_from_rust_target "$MEDIAFORGE_CARGO_TARGET")"
else
  built_binary="$cargo_target_dir/release/$binary_name"
  docker_arch="$(docker_arch_from_uname)"
fi

output_path="${MEDIAFORGE_PREBUILT_BINARY:-deploy/prebuilt/${docker_arch}/${binary_name}}"

cargo "${cargo_args[@]}"

install -d -m 0755 "$(dirname "$output_path")"
install -m 0755 "$built_binary" "$output_path"

printf 'Built prebuilt Docker binary: %s\n' "$output_path"
