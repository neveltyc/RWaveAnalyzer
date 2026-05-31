#!/usr/bin/env bash
# Build release `rwave` binaries for the supported deployment platforms.
#
#   target           Rust triple                    Output                            Linking
#   --------------   ----------------------------   -------------------------------   ----------
#   linux-amd64      x86_64-unknown-linux-musl      dist/rwave-linux-amd64            fully static
#   linux-arm64      aarch64-unknown-linux-musl     dist/rwave-linux-arm64            fully static
#   windows-amd64    x86_64-pc-windows-gnu          dist/rwave-windows-amd64.exe      MinGW (Rust stdlib only; no DLLs required)
#   macos-arm64      aarch64-apple-darwin           dist/rwave-macos-arm64            native (Apple Silicon)
#
# Linux & Windows targets are produced from any host via `cargo-zigbuild`
# (Zig as cross-linker). The macOS target MUST be built from a macOS host —
# cross-compiling to Darwin from Linux needs the Apple SDK and is
# deliberately not supported here. (Intel-Mac binaries are not shipped;
# Apple Silicon has been the only macOS arch worth supporting since 2023.)
#
#   one-time setup (macOS):
#     brew install rustup zig
#     export PATH="$(brew --prefix)/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
#     rustup default stable
#     cargo install --locked cargo-zigbuild
#     rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
#                       x86_64-pc-windows-gnu aarch64-apple-darwin
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
# the user's shell rc hasn't been re-sourced. Covers Apple Silicon
# (/opt/homebrew) and Intel mac (/usr/local) prefixes; no-op on other hosts.
for _d in /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin "${HOME}/.cargo/bin"; do
  if [ -d "$_d" ]; then
    case ":${PATH}:" in *":${_d}:"*) ;; *) PATH="${_d}:${PATH}" ;; esac
  fi
done
unset _d
export PATH

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

ALL_TARGETS=(linux-amd64 linux-arm64 windows-amd64 macos-arm64)
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
    macos-arm64)    echo "aarch64-apple-darwin" ;;
    *) return 1 ;;
  esac
}

output_for() {
  case "$1" in
    linux-amd64)    echo "dist/rwave-linux-amd64" ;;
    linux-arm64)    echo "dist/rwave-linux-arm64" ;;
    windows-amd64)  echo "dist/rwave-windows-amd64.exe" ;;
    macos-arm64)    echo "dist/rwave-macos-arm64" ;;
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
#   - macOS targets: must be built from a macOS host. Plain `cargo build`
#     (native dylib link); Apple Silicon host cross-builds x86_64-apple-darwin
#     via rustup's downloaded std lib, no extra tooling.
#   - Native Linux on the matching musl arch + musl-gcc available -> plain cargo build.
#   - Everything else (Linux/Windows musl from any host) -> cargo zigbuild.
need_zigbuild=0
for t in "${TARGETS[@]}"; do
  case "$t" in
    macos-*)
      if [ "$HOST_OS" != "Darwin" ]; then
        die "$t can only be built on a macOS host (cross-compile from $HOST_OS to Darwin needs the Apple SDK and is not supported)."
      fi
      ;;
    *)
      native=0
      if [ "$HOST_OS" = "Linux" ]; then
        case "$t-$HOST_ARCH" in
          linux-amd64-x86_64|linux-amd64-amd64)         have musl-gcc && native=1 ;;
          linux-arm64-aarch64|linux-arm64-arm64)        have musl-gcc && native=1 ;;
        esac
      fi
      if [ "$native" = "0" ]; then need_zigbuild=1; fi
      ;;
  esac
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
  case "$t" in
    macos-*)
      # native cargo build on a macOS host; no extra tooling needed.
      cmd=(cargo build)
      ;;
    *)
      local native=0
      if [ "$HOST_OS" = "Linux" ]; then
        case "$t-$HOST_ARCH" in
          linux-amd64-x86_64|linux-amd64-amd64)   have musl-gcc && native=1 ;;
          linux-arm64-aarch64|linux-arm64-arm64)  have musl-gcc && native=1 ;;
        esac
      fi
      if [ "$native" = "0" ]; then cmd=(cargo zigbuild); fi
      ;;
  esac

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
      macos-arm64-Darwin-arm64|macos-arm64-Darwin-aarch64) runnable=1 ;;
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
