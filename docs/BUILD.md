# Building `rwave`

`rwave` is pure Rust — no C code, no `build.rs`, no native dependencies — so the
only hard requirement is a Rust toolchain (>= 1.90, edition 2024). A linker is
needed only for the cross-compiled deployment binaries.

## Developing (macOS, Linux, anywhere)

Use the standard Cargo workflow for day-to-day development. This builds for your
own machine and needs nothing beyond Rust:

```sh
cargo build              # debug build
cargo run -- info file.fst
cargo test               # unit tests
cargo build --release    # optimized build for local use
```

On macOS (Apple Silicon or Intel) this just works after installing Rust from
<https://rustup.rs>. The resulting binary runs on your Mac; it is **not** a
Linux or Windows binary — for those, see below.

### Verification harnesses

```sh
bash verify/run.sh           # smoke + VCD/FST parity on bundled stimulus (no extra deps)
bash verify/differential.sh  # parity vs the reference Python tool (skips cleanly if absent)
```

## Producing deployable binaries

`scripts/build-release.sh` builds release binaries for the three supported
deployment platforms. It checks prerequisites up front and prints the exact
install command for anything missing.

| target          | Rust triple                   | output                            | linking                                          |
|-----------------|-------------------------------|-----------------------------------|--------------------------------------------------|
| `linux-amd64`   | `x86_64-unknown-linux-gnu`    | `dist/rwave-linux-amd64`          | glibc dynamic (manylinux2014 baseline, glibc ≥ 2.17); needs `dlopen` for the plugin path |
| `linux-arm64`   | `aarch64-unknown-linux-musl`  | `dist/rwave-linux-arm64`          | fully static; plugin path is compile-time disabled on aarch64 |
| `windows-amd64` | `x86_64-pc-windows-gnu`       | `dist/rwave-windows-amd64.exe`    | MinGW; no extra DLLs (Rust stdlib only)           |
| `macos-arm64`   | `aarch64-apple-darwin`        | `dist/rwave-macos-arm64`          | native Mach-O; built only from a macOS host (cross-build from Linux to Darwin needs the Apple SDK and is not supported) |

All three are produced by a single command and use the same cross-compilation
driver, so the recipe works identically from macOS, Linux, or any other host.

### One-time setup

The release builds are cross-compiled with **`cargo-zigbuild`** (Zig as the
cross-linker), avoiding fiddly per-target GCC toolchains.

**macOS (Apple Silicon or Intel):**

```sh
brew install rustup zig
export PATH="$(brew --prefix)/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
# also add the export above to ~/.zshrc so future shells pick it up

rustup default stable
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu \
                  aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu
```

**Linux:**

```sh
# rustup from https://rustup.rs (or your distro's package), then:
sudo apt-get install -y zig      # or: pip install ziglang
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu \
                  aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu
```

A native Linux host may build the matching-arch target with plain
`cargo build` and skip Zig: `linux-amd64` uses the system `gcc` linker
(always available on Linux hosts), `linux-arm64` needs `musl-gcc`
(Debian/Ubuntu: `apt-get install musl-tools`). The script auto-detects
either situation and only invokes Zig for genuinely cross targets.

### Building

```sh
scripts/build-release.sh                          # all three targets
scripts/build-release.sh --target linux-amd64    # one target
scripts/build-release.sh --target linux-arm64
scripts/build-release.sh --target windows-amd64
scripts/build-release.sh --target linux-amd64,windows-amd64
scripts/build-release.sh --run                   # smoke-test runnable outputs
```

Outputs land in `dist/` (git-ignored). The raw build tree under
`target/<triple>/release/rwave[.exe]` is equally valid.

`--run` invokes `<binary> --version` on any output that is runnable on the
current host — i.e. matching OS *and* architecture. Cross-built outputs are
silently skipped. To exercise a Linux binary from macOS, mount `dist/` into a
container of the target arch:

```sh
docker run --rm --platform linux/amd64 -v "$PWD/dist:/d:ro" \
  debian:bookworm-slim /d/rwave-linux-amd64 --version
docker run --rm --platform linux/arm64 -v "$PWD/dist:/d:ro" \
  alpine:3 /d/rwave-linux-arm64 --version
```

## Notes

- `linux-amd64` is glibc-dynamic (manylinux2014 baseline) so it can
  `dlopen` plugins. `linux-arm64` is fully static; the plugin path is
  compile-time gated off on aarch64.
- The Windows `.exe` needs no DLLs (Rust stdlib statically linked).
- Build artifacts go to `dist/` (git-ignored).
- `.cargo/config.toml` carries linker config for the musl target only;
  the gnu and Windows targets use system defaults. `cargo-zigbuild`
  supplies its own linker and ignores both.
