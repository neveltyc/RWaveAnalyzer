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
| `linux-amd64`   | `x86_64-unknown-linux-musl`   | `dist/rwave-linux-amd64`          | fully static; runs on any x86-64 Linux (incl. containers/Alpine) |
| `linux-arm64`   | `aarch64-unknown-linux-musl`  | `dist/rwave-linux-arm64`          | fully static; runs on any aarch64 Linux           |
| `windows-amd64` | `x86_64-pc-windows-gnu`       | `dist/rwave-windows-amd64.exe`    | MinGW; no extra DLLs (Rust stdlib only)           |

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
rustup target add x86_64-unknown-linux-musl \
                  aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu
```

**Linux:**

```sh
# rustup from https://rustup.rs (or your distro's package), then:
sudo apt-get install -y zig      # or: pip install ziglang
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-musl \
                  aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu
```

A native Linux host of matching arch may alternatively use `musl-gcc`
(Debian/Ubuntu: `apt-get install musl-tools`) and plain `cargo build` for the
local-arch Linux target; the script auto-detects this and skips Zig for that
target. The other two targets always go through Zig.

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
  alpine:3 /d/rwave-linux-amd64 --version
docker run --rm --platform linux/arm64 -v "$PWD/dist:/d:ro" \
  alpine:3 /d/rwave-linux-arm64 --version
```

Alpine ships only musl, so a successful run there is a stronger check than
glibc-based distros — it confirms the binary is genuinely static.

## Notes

- The static musl binaries have no runtime dependencies, which makes them the
  most portable choice for deployment and the recommended default for Linux.
- The Windows binary is built against `x86_64-pc-windows-gnu` (MinGW). Because
  `rwave` is pure Rust and the Rust stdlib is statically linked, the resulting
  `.exe` does not require MinGW DLLs at runtime.
- Build artifacts go to `dist/` (git-ignored).
- Linker configuration for the musl targets lives in `.cargo/config.toml` and
  is consulted by plain `cargo build` (any host, provided `musl-gcc` is on
  `PATH`); `cargo-zigbuild` supplies its own linker and ignores that setting.
