#!/usr/bin/env bash
# Build release binaries for distribution:
#   * Linux amd64, fully static (musl)         -> dist/rwave-linux-amd64
#   * Windows amd64 (MinGW-w64)                -> dist/rwave-windows-amd64.exe
#
# Prerequisites (one-time):
#   rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-gnu
#   # Debian/Ubuntu host packages:
#   sudo apt-get install -y musl-tools gcc-mingw-w64-x86-64
#
# Linker selection lives in .cargo/config.toml, so a plain `cargo build
# --target ...` picks up the right cross-linker automatically.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p dist

echo ">> Linux amd64 (static, musl)"
cargo build --release --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/rwave dist/rwave-linux-amd64
echo "   $(file dist/rwave-linux-amd64)"

echo ">> Windows amd64 (mingw)"
cargo build --release --target x86_64-pc-windows-gnu
cp target/x86_64-pc-windows-gnu/release/rwave.exe dist/rwave-windows-amd64.exe
echo "   $(file dist/rwave-windows-amd64.exe)"

echo ">> Done. Artifacts in dist/:"
ls -la dist/
