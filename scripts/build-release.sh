#!/usr/bin/env bash
# Build release `rwave` binaries for the supported deployment platforms.
#
#   target           Rust triple                    Output                            Linking
#   --------------   ----------------------------   -------------------------------   ----------
#   linux-amd64      x86_64-unknown-linux-musl      dist/rwave-linux-amd64            fully static
#   linux-arm64      aarch64-unknown-linux-musl     dist/rwave-linux-arm64            fully static
#   windows-amd64    x86_64-pc-windows-gnu          dist/rwave-windows-amd64.exe      MinGW (Rust stdlib only; no DLLs required)
#
# Cross-compilation is unified under `cargo-zigbuild` (Zig as cross-linker), so
# the same recipe works from macOS, Linux, or any other host:
#
#   one-time setup (macOS):
#     brew install rustup zig
#     export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
#     rustup default stable
#     cargo install --locked cargo-zigbuild
#     rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                       x86_64-pc-windows-gnu
#
# A native Linux host that prefers GCC over Zig can still build the
# matching-arch musl target with plain `cargo build` provided `musl-gcc` is
# installed (Debian/Ubuntu: `musl-tools`). The cross targets always go through
# Zig.
#
# Usage:
#   scripts/build-release.sh                              # all three targets
#   scripts/build-release.sh --target linux-amd64        # one target
#   scripts/build-release.sh --target linux-amd64,windows-amd64
#   scripts/build-release.sh --run                       # smoke-test runnable outputs
#
# The script checks its prerequisites and prints exact install commands for
# anything missing, instead of failing partway with a cryptic linker error.
set -euo pipefail

cd "$(dirname "$0")/.."

# Make Homebrew's keg-only rustup and the cargo bin dir discoverable even when
# the user's shell rc hasn't been re-sourced.
export PATH="/opt/homebrew/opt/rustup/bin:${HOME}/.cargo/bin:${PATH}"

# ---- args ----------------------------------------------------------------
TARGETS_INPUT=""
RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGETS_INPUT="${2:-}"; shift 2 ;;
    --target=*) TARGETS_INPUT="${1#*=}"; shift ;;
    --run) RUN=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ALL_TARGETS=(linux-amd64 linux-arm64 windows-amd64)
if [ -z "$TARGETS_INPUT" ]; then
  TARGETS=("${ALL_TARGETS[@]}")
else
  IFS=',' read -r -a TARGETS <<< "$TARGETS_INPUT"
fi

triple_for() {
  case "$1" in
    linux-amd64)    echo "x86_64-unknown-linux-musl" ;;
    linux-arm64)    echo "aarch64-unknown-linux-musl" ;;
    windows-amd64)  echo "x86_64-pc-windows-gnu" ;;
    *) return 1 ;;
  esac
}

output_for() {
  case "$1" in
    linux-amd64)    echo "dist/rwave-linux-amd64" ;;
    linux-arm64)    echo "dist/rwave-linux-arm64" ;;
    windows-amd64)  echo "dist/rwave-windows-amd64.exe" ;;
    *) return 1 ;;
  esac
}

binary_for() {
  case "$1" in
    windows-amd64)  echo "target/$(triple_for "$1")/release/rwave.exe" ;;
    *)              echo "target/$(triple_for "$1")/release/rwave" ;;
  esac
}

# Validate target names up front so a typo fails fast.
for t in "${TARGETS[@]}"; do
  triple_for "$t" >/dev/null || { echo "unknown target '$t' (expected: ${ALL_TARGETS[*]})" >&2; exit 2; }
done

info() { printf '>> %s\n' "$*"; }
ok()   { printf '   %s\n' "$*"; }
die()  { printf 'XX %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

HOST_OS="$(uname -s)"     # Linux | Darwin
HOST_ARCH="$(uname -m)"   # x86_64 | arm64 | aarch64

# ---- prerequisite checks -------------------------------------------------
have cargo  || die "cargo not found. Install Rust (https://rustup.rs) and re-run."
have rustup || info "rustup not found; assuming a non-rustup Rust. Ensure the required targets are available."

# Decide the build driver per target.
#   - Native Linux on the matching musl arch + musl-gcc available -> plain cargo build.
#   - Everything else -> cargo zigbuild (uniform cross story).
need_zigbuild=0
for t in "${TARGETS[@]}"; do
  triple="$(triple_for "$t")"
  native=0
  if [ "$HOST_OS" = "Linux" ]; then
    case "$t-$HOST_ARCH" in
      linux-amd64-x86_64|linux-amd64-amd64)         have musl-gcc && native=1 ;;
      linux-arm64-aarch64|linux-arm64-arm64)        have musl-gcc && native=1 ;;
    esac
  fi
  if [ "$native" = "0" ]; then need_zigbuild=1; fi
done

if [ "$need_zigbuild" = "1" ]; then
  have cargo-zigbuild || die "cargo-zigbuild not found. Install it with:
     cargo install --locked cargo-zigbuild"
  if ! have zig && ! python3 -c 'import ziglang' >/dev/null 2>&1; then
    die "zig not found. Install it:
     macOS:  brew install zig
     other:  https://ziglang.org/download/  (or: pip install ziglang)"
  fi
fi

# Ensure required Rust targets' std libs are present (rustup only).
if have rustup; then
  installed="$(rustup target list --installed 2>/dev/null || true)"
  for t in "${TARGETS[@]}"; do
    triple="$(triple_for "$t")"
    if ! printf '%s\n' "$installed" | grep -qx "$triple"; then
      info "Adding Rust target $triple ..."
      rustup target add "$triple"
    fi
  done
fi

mkdir -p dist

# ---- build loop ----------------------------------------------------------
build_one() {
  local t="$1"
  local triple; triple="$(triple_for "$t")"
  local bin; bin="$(binary_for "$t")"
  local out; out="$(output_for "$t")"

  # Pick driver for THIS target.
  local -a cmd=(cargo build)
  local native=0
  if [ "$HOST_OS" = "Linux" ]; then
    case "$t-$HOST_ARCH" in
      linux-amd64-x86_64|linux-amd64-amd64)   have musl-gcc && native=1 ;;
      linux-arm64-aarch64|linux-arm64-arm64)  have musl-gcc && native=1 ;;
    esac
  fi
  if [ "$native" = "0" ]; then cmd=(cargo zigbuild); fi

  info "Building $t  ($triple, driver: ${cmd[*]}) ..."
  "${cmd[@]}" --release --target "$triple"

  [ -f "$bin" ] || die "build reported success but $bin is missing."
  cp "$bin" "$out"

  ok "Binary: $out"
  if have file; then ok "$(file "$out")"; fi
  local kb; kb=$(( $(wc -c < "$out") / 1024 ))
  ok "Size:   ${kb} KB"
}

for t in "${TARGETS[@]}"; do
  build_one "$t"
done

# ---- optional smoke test -------------------------------------------------
if [ "$RUN" = "1" ]; then
  for t in "${TARGETS[@]}"; do
    out="$(output_for "$t")"
    runnable=0
    case "$t-$HOST_OS-$HOST_ARCH" in
      linux-amd64-Linux-x86_64|linux-amd64-Linux-amd64)   runnable=1 ;;
      linux-arm64-Linux-aarch64|linux-arm64-Linux-arm64)  runnable=1 ;;
    esac
    if [ "$runnable" = "1" ]; then
      info "Smoke test: $out --version"
      "$out" --version
    else
      info "Skipping --run for $t: not runnable on $HOST_OS/$HOST_ARCH."
    fi
  done
fi

info "Done."
