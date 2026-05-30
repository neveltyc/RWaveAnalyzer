# Building `rwave`

`rwave` is pure Rust — no C code, no `build.rs`, no native dependencies — so the
only hard requirement is a Rust toolchain (>= 1.90, edition 2024). A linker is
needed only for the special targets used to produce deployment binaries.

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
Linux binary — for that, see below.

### Verification harnesses

```sh
bash verify/run.sh           # smoke + VCD/FST parity on bundled stimulus (no extra deps)
bash verify/differential.sh  # parity vs the reference Python tool (skips cleanly if absent)
```

## Producing a deployable Linux x86-64 binary

`scripts/build-release.sh` builds a release binary targeting Linux x86-64. It
checks prerequisites up front and prints the exact install command for anything
missing.

Two flavours:

| flavour            | target                      | output                          | notes |
|--------------------|-----------------------------|---------------------------------|-------|
| `static` (default) | `x86_64-unknown-linux-musl` | `dist/rwave-linux-amd64`        | fully static; runs on any x86-64 Linux (incl. containers/Alpine) |
| `glibc`            | `x86_64-unknown-linux-gnu`  | `dist/rwave-linux-amd64-glibc`  | dynamically linked; smaller; needs a compatible system glibc |

### From a Linux x86-64 host (native)

```sh
# static flavour needs the musl C toolchain (one-time):
sudo apt-get install -y musl-tools          # Debian/Ubuntu
rustup target add x86_64-unknown-linux-musl  # one-time

scripts/build-release.sh                 # -> dist/rwave-linux-amd64 (static)
scripts/build-release.sh --flavour glibc # -> dist/rwave-linux-amd64-glibc
scripts/build-release.sh --run           # build, then print --version
```

### Cross-building from macOS (Apple Silicon) → Linux x86-64

Cross-compiling to Linux is easiest with **`cargo-zigbuild`**, which uses Zig as
the cross-linker so you don't need a fiddly cross-GCC. One-time setup:

```sh
brew install zig
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-musl   # static (default)
# or: rustup target add x86_64-unknown-linux-gnu   # for --flavour glibc
```

Then:

```sh
scripts/build-release.sh --zig                  # -> dist/rwave-linux-amd64 (static)
scripts/build-release.sh --zig --flavour glibc  # -> dist/rwave-linux-amd64-glibc
```

The cross-built binary is a Linux executable, so it won't run on the Mac itself
(`--run` is skipped automatically when cross-building); copy it to the target
Linux host to use it.

> If you don't have Homebrew, Zig can also come from `pip install ziglang`
> (the script accepts either). See <https://ziglang.org/download/>.

## Notes

- The fully-static musl binary has no runtime dependencies, which makes it the
  most portable choice for deployment and the recommended default.
- Build artifacts go to `dist/` (git-ignored). The raw build tree under
  `target/<triple>/release/rwave` is equally valid.
- Linker configuration for the musl target lives in `.cargo/config.toml`.
  `cargo-zigbuild` supplies its own linker and ignores that setting.
