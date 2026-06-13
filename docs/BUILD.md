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

## Producing the release binaries

`scripts/build-release.sh` builds the four deployment binaries. It checks
prerequisites up front and prints the exact install command for anything
missing.

| target          | Rust triple                  | output                         | notes |
|-----------------|------------------------------|--------------------------------|-------|
| `linux-amd64`   | `x86_64-unknown-linux-gnu`   | `dist/rwave-linux-amd64`       | glibc-dynamic (manylinux2014, glibc ≥ 2.17); core + WLF + FSDB |
| `windows-amd64` | `x86_64-pc-windows-gnu`      | `dist/rwave-windows-amd64.exe` | MinGW; core only |
| `linux-arm64`   | `aarch64-unknown-linux-musl` | `dist/rwave-linux-arm64`       | fully static; VCD/FST/GHW core only |
| `macos-arm64`   | `aarch64-apple-darwin`       | `dist/rwave-macos-arm64`       | native; core only (built from a macOS host) |

The experimental WLF/FSDB backends are target-gated to amd64 linux, so
building each target with default features yields the right feature set
automatically. `linux-amd64` is dynamically linked so it can `dlopen` the
vendor library; the other binaries are core-only. The VCD/FST/GHW core also builds from source on any
platform via plain `cargo build`.

### One-time setup

Cross-compiled with **`cargo-zigbuild`** (Zig as the cross-linker), so the
recipe works from macOS, Linux, or any host — except the macOS target, which
needs a macOS host.

**macOS (Apple Silicon or Intel):**

```sh
brew install rustup zig
export PATH="$(brew --prefix)/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
# also add the export above to ~/.zshrc so future shells pick it up

rustup default stable
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu aarch64-apple-darwin
```

**Linux:**

```sh
# rustup from https://rustup.rs (or your distro's package), then:
sudo apt-get install -y zig      # or: pip install ziglang
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu
# (the macOS target can only be built on a macOS host)
```

### Building

```sh
scripts/build-release.sh                        # all four targets
scripts/build-release.sh --target linux-amd64   # one target
scripts/build-release.sh --run                  # smoke-test runnable outputs
```

Outputs land in `dist/` (git-ignored); the raw build tree under
`target/<triple>/release/rwave[.exe]` is equally valid. `--run` invokes
`--version` only on a matching host. To exercise a Linux binary from macOS,
run it in a container:

```sh
docker run --rm --platform linux/amd64 -v "$PWD/dist:/d:ro" \
  debian:bookworm-slim /d/rwave-linux-amd64 --version
```

## Notes

- `linux-amd64` and `windows-amd64` are dynamically linked so they can
  `dlopen` the WLF/FSDB vendor libraries (and external plugins); `linux-arm64`
  is musl-static and `macos-arm64` native, both core-only. The linux-amd64
  glibc baseline is pinned to 2.17 via zigbuild's `.2.17` suffix, so it runs
  on every mainstream Linux from 2014 on.
- WLF/FSDB are experimental and target-gated to amd64 linux, behind default-on
  `wlf`/`fsdb` features. The VCD/FST/GHW core builds anywhere with `cargo build`.
- Build artifacts go to `dist/` (git-ignored).
