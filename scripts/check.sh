#!/usr/bin/env bash
# Local CI: formatting, lints, tests, and release size report.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
if [[ "${1:-}" == "--size" ]]; then
  CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --target x86_64-unknown-linux-musl -p dman --quiet
  bin=target/x86_64-unknown-linux-musl/release/dman
  printf 'dman release (musl, stripped): %s bytes (%.2f MiB)\n' \
    "$(stat -c %s "$bin")" "$(echo "$(stat -c %s "$bin") / 1048576" | bc -l)"
fi
