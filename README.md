<p align="center">
  <h1 align="center">RWaveAnalyzer</h1>
  <p align="center">
    A fast, single-binary CLI for inspecting RTL simulation waveforms &mdash;
    <b>VCD</b>, <b>FST</b>, and <b>GHW</b>, with experimental support for <b>WLF</b> and <b>FSDB</b> &mdash;
    built for RTL debug, CI, and AI agents.
  </p>
</p>

<p align="center">
  <img alt="Release" src="https://img.shields.io/github/v/release/neveltyc/RWaveAnalyzer?sort=semver&style=flat-square&color=3366cc">
  <img alt="CI" src="https://img.shields.io/github/actions/workflow/status/neveltyc/RWaveAnalyzer/ci.yml?branch=main&style=flat-square&label=CI">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-3366cc?style=flat-square">
</p>

---

## Why RWaveAnalyzer?

You have a multi-gigabyte FST from an overnight regression, and you need to know
exactly when `arvalid` and `arready` were both high, or what `state[3:0]` held at
17.55 µs. Opening Verdi or GTKWave means waiting for a GUI to start, clicking
down the hierarchy, and reading values off a cursor. RWaveAnalyzer answers the
same questions from the terminal, in a single command:

```sh
rwave search sim.fst --condition 'arvalid=1,arready=1' --show araddr,arlen
```

The tool is a single self-contained binary called `rwave`. It reads the open
**VCD**, **FST**, and **GHW** formats, and on linux-amd64 it adds experimental
support for the **WLF** (Mentor/Questa) and **FSDB** (Synopsys/Verdi) databases,
which it reads through each vendor's own library (see [WLF & FSDB](#wlf--fsdb)).
Every command also has a `--json` mode with stable keys, so the same tool drives
a human at a prompt, a CI gate, and an AI agent equally well. Whole-file commands
stream their work in bounded memory, so a dump with hundreds of thousands of
signals does not exhaust RAM.

## Quick start

Point any command at a `.vcd`, `.fst`, `.ghw` (or `.wlf` / `.fsdb`) file:

```sh
# What's in this file?
rwave info sim.fst

# Show me the clock and reset
rwave list sim.fst --filter clk,rst

# What happened between 100 ns and 200 ns?
rwave dump sim.fst --begin 100ns --end 200ns --filter state

# When were valid and ready both high?
rwave search sim.fst --condition 'valid=1,ready=1' --show data

# What are all known values at exactly 17.55 us?
rwave snapshot sim.fst --at 17.55us --filter state,init_done

# What changed between two times?
rwave compare sim.fst --at 17.5us,17.7us --filter bus

# Which signals are active versus static?
rwave summary sim.fst --filter alu
```

Add `--json` to any command for compact, machine-readable output.

## Install

Download the `rwave` binary for your platform from the
[latest release](https://github.com/neveltyc/RWaveAnalyzer/releases/latest):

| Platform | Binary | VCD · FST · GHW | WLF | FSDB |
|:--|:--|:--:|:--:|:--:|
| Linux x86-64          | `rwave-linux-amd64`       | ✓ | ✓ | ✓ |
| Linux ARM64           | `rwave-linux-arm64`       | ✓ | — | — |
| Windows x86-64        | `rwave-windows-amd64.exe` | ✓ | — | — |
| macOS (Apple Silicon) | `rwave-macos-arm64`       | ✓ | — | — |

```sh
curl -fsSL -o rwave \
  https://github.com/neveltyc/RWaveAnalyzer/releases/latest/download/rwave-linux-amd64
chmod +x rwave
./rwave --version
```

Every binary reads VCD/FST/GHW; WLF and FSDB are linux-amd64 only (see
[WLF & FSDB](#wlf--fsdb)). The `rwave-linux-amd64` build is dynamically linked
against glibc with a 2.17 baseline (manylinux2014), so it runs on every
mainstream Linux distribution released since 2014.

## Building from source

The only requirement for a local build is a recent stable Rust toolchain
(developed against 1.90, edition 2024). The build is pure Rust — there is no C
code, no `build.rs`, and no system dependency to install — so a plain `cargo`
invocation produces a binary for the host machine:

```sh
cargo build --release      # → target/release/rwave
```

The WLF and FSDB backends are gated behind the default-on `wlf` and `fsdb`
features and are further restricted to `x86_64` Linux at compile time; on any
other host they compile out and you are left with the VCD/FST/GHW core.
`--no-default-features` forces that pure core on any platform. The parser
front-end (`wellen`) and its FST reader are vendored under `vendor/`, so the
build needs no network access and always uses the exact, pinned parser revision.

To produce the four release binaries, `scripts/build-release.sh` cross-compiles
them with [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) (Zig
as the cross-linker), so the same recipe works from any host — only the macOS
target requires a macOS machine. Each target receives the correct feature set
automatically, and `linux-amd64` is pinned to the glibc 2.17 baseline.

| Target | Triple | Output |
|:--|:--|:--|
| `linux-amd64`   | `x86_64-unknown-linux-gnu`   | `dist/rwave-linux-amd64`       |
| `linux-arm64`   | `aarch64-unknown-linux-musl` | `dist/rwave-linux-arm64`       |
| `windows-amd64` | `x86_64-pc-windows-gnu`      | `dist/rwave-windows-amd64.exe` |
| `macos-arm64`   | `aarch64-apple-darwin`       | `dist/rwave-macos-arm64`       |

```sh
# one-time setup (macOS)
brew install rustup zig
rustup default stable
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu aarch64-apple-darwin

./scripts/build-release.sh                        # all four targets
./scripts/build-release.sh --target linux-amd64   # a single target
```

The script checks its prerequisites up front and prints the exact install
command for anything that is missing. [docs/BUILD.md](docs/BUILD.md) covers the
cross-compilation setup, the per-target linking choices, and the Linux recipe in
full.

## Commands

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
```

| Command | What it does |
|:--|:--|
| `info`     | Timescale, signal and type counts, time span, and scopes — the file at a glance |
| `list`     | Enumerate signals with path, width, and type (`--filter` matches any alias) |
| `dump`     | Print every value change in a time window, in time order |
| `summary`  | Per-signal statistics: active versus static, change count, rise/fall edges |
| `snapshot` | All known signal values at one time point (`--at T`) |
| `compare`  | What changed between two time points (`--at T1,T2`) |
| `search`   | Find the intervals where a condition holds, optionally watching related signals |

Every command accepts a `--begin`/`--end` time window and a `--filter`. Times
take the unit suffixes `fs`, `ps`, `ns`, `us`, `ms`, and `s` (for example
`17.5us`); a bare integer is interpreted as raw ticks. Filters are
comma-separated and match by substring or `*`-glob. The global flags are `--json`
for structured output, `--limit N` to cap the number of rows (the default is
200, and `0` means unlimited), and `--verbose` for extra fields. A search
condition is a comma-separated AND-list of `SIG=VAL` or `SIG!=VAL` terms, with
values written in decimal, hexadecimal (`0xff`), binary (`b1010`), or 4-state.
Run `rwave <command> --help` for the complete reference.

## JSON output

Under `--json`, every command emits compact structured JSON. Each time is given
both as a raw tick count (the `*_ticks` fields) and in human-readable form (the
`*_h` fields), so the output is equally usable by a script, a CI gate, or an AI
agent rather than only by a person reading the terminal:

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition 'state=5' --show data
```

## WLF & FSDB

In addition to the open formats, RWaveAnalyzer has experimental support for two
vendor waveform databases on linux-amd64: Mentor/Siemens **WLF**, written by
Questa and ModelSim, and Synopsys **FSDB**, written by Verdi. It reads each one
through the vendor's own reader library, so there is no separate `wlf2vcd` or
`fsdb2vcd` conversion step and no intermediate file to keep around.

Because rwave calls into the vendor library, that library has to be available on
the machine. rwave does not ship it; instead, you point rwave at the copy that
comes with your own licensed tool installation, using an environment variable:

| Format | Vendor | Reader library | Environment variable |
|:--|:--|:--|:--|
| `.wlf`  | Mentor/Siemens Questa, ModelSim | `libwlf.so` | `RWAVE_WLF_LIB`  |
| `.fsdb` | Synopsys Verdi                  | `libNPI.so` | `RWAVE_FSDB_LIB` |

Set the variable to the absolute path of the library, and then run any command
exactly as you would for a VCD or FST file:

```sh
# WLF — libwlf.so from your Questa / ModelSim installation
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf

# FSDB — libNPI.so from your Verdi installation
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
rwave info sim.fsdb
```

This support is experimental and limited to linux-amd64. The vendor's tool must
be installed and licensed on the same machine — FSDB in particular needs a
Verdi-Ultra license feature — and for FSDB you should source your Verdi
environment first, so that `libNPI.so` can locate `$VERDI_HOME` and its own
dependent libraries. Configuring and licensing the vendor software is outside
rwave's control.

If you need a reader for some other format, or a different implementation of one
of these, rwave will load any backend that implements its C ABI from
`$RWAVE_PLUGIN_<EXT>`. That interface is documented in
[docs/PLUGIN.md](docs/PLUGIN.md).

### Disclaimer

RWaveAnalyzer reads WLF and FSDB only through each vendor's own public reader
interface. It contains no proprietary binaries and no vendor source code, links
against none of them at build time, and redistributes no vendor software; at run
time it loads the reader library that you supply from your own licensed
installation. Reading these formats therefore requires the vendor's software and
a valid license on your machine, and obtaining and configuring those under the
vendor's terms is your responsibility.

## For AI agents

The repository ships an agent skill at [skill/SKILL.md](skill/SKILL.md): a
decision tree that maps user intent to a command, a cheat sheet of the JSON
fields, the condition grammar, the WLF/FSDB setup, and a handful of debugging
workflows. Point your agent at it, and the `--json` output of every command does
the rest.

## Architecture

The crate is layered top to bottom, and each layer depends only on the ones
below it:

```
        cli            argument parsing only
         │
      commands         per-command logic and presentation (text / JSON)
         │
       model           format-neutral domain: signal table, replay, snapshots
         │
      backend          WaveformBackend trait (the parser contract)
         │
  wellen_backend       the only code that touches the wellen parser
```

The decisive boundary is the **`WaveformBackend`** trait. A backend hands the
model fully decoded, owned per-signal traces (parallel time and value arrays);
the model owns all of the replay, merging, and snapshot logic and works purely
over slices. Because the trait surface is coarse — there is no per-sample virtual
call — the hot path stays monomorphic, and adding a parser means adding a single
file under `backend/`. The vendor and plugin formats enter through that same
boundary: a backend can come from a vtable compiled into the binary
(`plugin/builtin/`) or from a `dlopen`ed library (`plugin/loader.rs`), and either
one is driven through `plugin_backend.rs` and the C ABI in
[`crates/rwave/include/rwave_backend.h`](crates/rwave/include/rwave_backend.h).

At the top level the repository is organized as follows:

```
crates/rwave/      the rwave crate (CLI, model, backends, plugin ABI)
vendor/            vendored parser front-end: wellen + a patched fst-reader
verify/            self-test and differential harnesses with committed stimulus
scripts/           release build and stimulus-generation scripts
skill/             the agent-skill descriptor
docs/              extended documentation (BUILD, PLUGIN)
.github/workflows/ CI (ci.yml), release (release.yml), and benchmark (bench.yml)
```

## Performance

- **Replay** is a binary min-heap k-way merge over the selected signals' traces,
  `O(n log k)` for `n` changes across `k` signals; ties within one timestamp
  resolve to writer (declaration) order.
- **Snapshots and `compare`** binary-search each signal for the last value at or
  before the target time, with no full replay.
- **Whole-file commands** — `summary`, and unfiltered `dump`/`snapshot`/`compare`
  — decode signals in memory-bounded batches and release each batch as they go,
  so peak memory is proportional to one batch rather than to the whole file.
  `summary` computes its per-signal statistics directly from each trace in an
  allocation-light loop, and `dump` keeps only the earliest `--limit` events in a
  bounded heap.

These streaming paths produce byte-identical output to the simple eager paths;
the switch between them is purely a memory and throughput optimization keyed on
how many signals were selected.

## Testing

```sh
cargo test                  # unit tests: formatting, filters, conditions, CLI
bash verify/run.sh          # smoke test plus VCD/FST parity on bundled stimulus
bash verify/differential.sh # behavioral parity against the reference Python tool
```

`verify/run.sh` needs only the built binary: it confirms that every command runs
on both a VCD and an FST, and that the value-bearing commands produce identical
results across the two formats for the same design. `verify/differential.sh`
compares rwave against the reference `vcd_analyzer.py` across all seven commands
on the fixtures and edge-case designs. It locates the reference through
`$VCD_ANALYZER`, a sibling checkout, or `$PATH`, and skips cleanly when none is
present, so it is safe to run in a fresh clone or in CI.

## Compatibility with VCD_ANALYZER

rwave's command-line surface mirrors the reference Python tool
[VCD_ANALYZER](https://github.com/neveltyc/VCD_ANALYZER), so the two are
interchangeable at the CLI. A few differences are intentional and follow from
using a real parser front-end and from generalizing beyond VCD:

- **Format-neutral wording.** Messages that named "VCD" specifically are
  generalized (for example, `cannot open waveform file`), because rwave handles
  several formats.
- **`list --verbose` identifier field.** The reference prints the raw VCD
  identifier code; rwave reports the backend's signal index instead, because the
  abstract backend does not expose VCD identifier codes.
- **Comments and synthesized buses.** The `wellen` reader does not preserve VCD
  `$comment` blocks, so `comments` is always empty, and `synthesized_buses` is
  reported as `0`.
- **FST conversion artifact.** `vcd2fst` does not carry Verilog
  `parameter`/`localparam` *values* into the FST, so a constant that shows a value
  in the VCD will show none in the converted FST. rwave faithfully reports
  whatever each file actually contains; this is a property of the conversion
  tool, not of rwave.
- **`dump` ordering within one timestamp.** When several signals change at the
  same time, the reference emits them in the order they physically appear in the
  VCD, whereas rwave orders them by declaration. The values, timestamps, and the
  set of emitted events are identical — only the relative order of events that
  share a timestamp can differ, and only for `dump`.

## License

MIT — see [LICENSE](LICENSE). The vendored components keep their own licenses:
`vendor/wellen` and `vendor/fst-reader` are both BSD-3-Clause.
