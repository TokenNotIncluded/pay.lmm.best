#!/usr/bin/env bash
# Native musl builds: each architecture uses its own runner, not QEMU.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  x86_64-unknown-linux-musl) arch=x86_64 ;;
  aarch64-unknown-linux-musl) arch=aarch64 ;;
  *) echo 'Usage: packaging/build.sh TARGET [--test]' >&2; exit 2 ;;
esac
[[ ${2:-} == '' || ${2:-} == --test ]] || exit 2
target=$1
[[ $(uname -s) == Linux && $(uname -m) == "$arch" ]] || { echo 'Use a native Linux runner matching the target.' >&2; exit 2; }
for tool in cargo rustup musl-gcc protoc python3; do command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }; done
rustup target add "$target"
key=${target//-/_}
export "CC_${key}=musl-gcc"
export "CARGO_TARGET_${key^^}_LINKER=musl-gcc"
# Target-scoped: build scripts and proc macros still run on the GNU host.
export "CARGO_TARGET_${key^^}_RUSTFLAGS=-C target-feature=+crt-static --remap-path-prefix=$PWD=/src/pay-lmm"
cargo build --locked --release --target "$target"
python3 packaging/package.py check --target "$target" --binary "target/$target/release/pay-lmm"
if [[ ${2:-} == --test ]]; then
  cargo test --locked --all-targets --target "$target"
  cargo clippy --locked --all-targets --target "$target" -- -D warnings
fi
