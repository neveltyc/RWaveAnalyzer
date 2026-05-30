#!/usr/bin/env bash
# Build a release `rwave` binary for deployment on Linux x86-64.
#
# Two flavours, selected with --flavour (default: static):
#   static  -> x86_64-unknown-linux-musl, fully static (no libc dependency).
#              Runs on any x86-64 Linux, including minimal/musl/container images.
#              -> dist/rwave-linux-amd64
#   glibc   -> x86_64-unknown-linux-gnu, dynamically linked against the system
#              glibc. Smaller, but needs a compatible glibc at runtime.
#              -> dist/rwave-linux-amd64-glibc
#
# Works in two scenarios:
#   * Building natively on a Linux x86-64 host.
#   * Cross-building from another host (e.g. macOS arm64) to Linux x86-64.
#     The static/musl flavour cross-builds cleanly with `cargo-zigbuild`
#     (recommended: it bundles the cross-linker via Zig). Pass --zig to use it.
#
# This script checks its prerequisites up front and prints exact install
# commands for anything missing, instead of failing partway with a cryptic
# linker error.
#
# Usage:
#   scripts/build-release.sh                # static musl binary (native or --zig)
#   scripts/build-release.sh --flavour glibc
#   scripts/build-release.sh --zig          # cross-build static from macOS/other
#   scripts/build-release.sh --run          # smoke-test the result (native only)
set -euo pipefail

cd "$(dirname "$0")/.."

# ---- args ----------------------------------------------------------------
FLAVOUR="static"   # static | glibc
USE_ZIG=0
RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --flavour) FLAVOUR="${2:-}"; shift 2 ;;
    --flavour=*) FLAVOUR="${1#*=}"; shift ;;
    --zig) USE_ZIG=1; shift ;;
    --run) RUN=1; shift ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$FLAVOUR" in
  static) TARGET="x86_64-unknown-linux-musl"; OUT="dist/rwave-linux-amd64" ;;
  glibc)  TARGET="x86_64-unknown-linux-gnu";  OUT="dist/rwave-linux-amd64-glibc" ;;
  *) echo "invalid --flavour '$FLAVOUR' (expected: static | glibc)" >&2; exit 2 ;;
esac

info() { printf '>> %s\n' "$*"; }
ok()   { printf '   %s\n' "$*"; }
die()  { printf 'XX %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# Detect host OS/arch so we can give the right install hints.
HOST_OS="$(uname -s)"   # Linux | Darwin
HOST_ARCH="$(uname -m)" # x86_64 | arm64 | aarch64

# ---- prerequisite checks -------------------------------------------------
have cargo || die "cargo not found. Install Rust from https://rustup.rs and re-run."
have rustup || info "rustup not found; assuming a non-rustup Rust. Ensure target '$TARGET' is available."

# Are we already on Linux x86-64 (native), or cross-building?
NATIVE_LINUX_X64=0
if [ "$HOST_OS" = "Linux" ] && { [ "$HOST_ARCH" = "x86_64" ] || [ "$HOST_ARCH" = "amd64" ]; }; then
  NATIVE_LINUX_X64=1
fi

# Decide on the build driver.
#   - Native Linux x86-64: plain `cargo build` (musl needs musl-gcc; glibc is built-in).
#   - Cross from another host: require --zig (cargo-zigbuild) -- the least painful
#     cross story; we refuse to guess at a hand-configured cross-GCC.
BUILD_CMD=(cargo build)
if [ "$USE_ZIG" = "1" ]; then
  have cargo-zigbuild || die "cargo-zigbuild not found. Install it with:
     cargo install --locked cargo-zigbuild
   and install Zig (the cross-linker it uses):
     macOS:  brew install zig
     other:  see https://ziglang.org/download/  (or: pip install ziglang)"
  if ! have zig && ! python3 -c 'import ziglang' >/dev/null 2>&1; then
    die "zig not found. Install it (macOS: 'brew install zig'; or 'pip install ziglang')."
  fi
  BUILD_CMD=(cargo zigbuild)
  ok "Cross-build driver: cargo-zigbuild (Zig linker)."
elif [ "$NATIVE_LINUX_X64" = "1" ]; then
  if [ "$FLAVOUR" = "static" ] && ! have musl-gcc && ! have x86_64-linux-musl-gcc; then
    die "musl C toolchain not found (needed for the static/musl build).
   Install it:
     Debian/Ubuntu: sudo apt-get install -y musl-tools
     Fedora:        sudo dnf install -y musl-gcc
     Alpine:        apk add musl-dev
   Or build the dynamic flavour:  scripts/build-release.sh --flavour glibc
   Or cross-build with Zig:       scripts/build-release.sh --zig"
  fi
  ok "Native Linux x86-64 build."
else
  die "Building for Linux x86-64 from a $HOST_OS/$HOST_ARCH host requires cross-compilation.
   Re-run with --zig (recommended):
     cargo install --locked cargo-zigbuild
     brew install zig           # macOS;  or 'pip install ziglang'
     scripts/build-release.sh --zig
   (--zig works for the default static flavour and for --flavour glibc.)"
fi

# Ensure the Rust target's std library is installed (rustup only).
if have rustup; then
  if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    info "Adding Rust target $TARGET ..."
    rustup target add "$TARGET"
  fi
fi

# ---- build ---------------------------------------------------------------
mkdir -p dist
info "Building rwave (release, $FLAVOUR -> $TARGET) ..."
"${BUILD_CMD[@]}" --release --target "$TARGET"

BIN="target/$TARGET/release/rwave"
[ -f "$BIN" ] || die "build reported success but $BIN is missing."
cp "$BIN" "$OUT"

ok "Binary: $OUT"
if have file; then ok "$(file "$OUT")"; fi
SIZE_KB=$(( $(wc -c < "$OUT") / 1024 ))
ok "Size:   ${SIZE_KB} KB"

# Smoke test only makes sense if the binary is runnable on this host.
if [ "$RUN" = "1" ]; then
  if [ "$NATIVE_LINUX_X64" = "1" ]; then
    info "Smoke test: $OUT --version"
    "$OUT" --version
  else
    info "Skipping --run: cross-built Linux binary is not runnable on this $HOST_OS host."
  fi
fi

info "Done."
