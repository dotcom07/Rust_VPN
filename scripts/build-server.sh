#!/usr/bin/env bash
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-musl}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  echo "Usage: TARGET=x86_64-unknown-linux-musl scripts/build-server.sh"
  exit 0
fi

rustup target add "$TARGET"

if command -v cargo-zigbuild >/dev/null 2>&1; then
  cargo zigbuild --release --target "$TARGET" -p litevpn-server
else
  cargo build --release --target "$TARGET" -p litevpn-server
fi

echo "target/$TARGET/release/litevpn-server"
