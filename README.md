# rwave

> A fast, headless waveform analyzer for RTL simulation — built for scripting and AI agents.

[![Release](https://img.shields.io/github/v/release/neveltyc/RWaveAnalyzer?sort=semver)](https://github.com/neveltyc/RWaveAnalyzer/releases/latest)
[![CI](https://github.com/neveltyc/RWaveAnalyzer/actions/workflows/ci.yml/badge.svg)](https://github.com/neveltyc/RWaveAnalyzer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`rwave` answers questions about a simulation dump from the command line — what's
inside, which signals exist, what changed between two times, when a condition
holds — and prints either readable text or stable JSON. It reads **VCD**,
**FST**, and **GHW** out of the box (plus **WLF** and **FSDB** on linux-amd64),
so one small binary replaces a click-through waveform viewer for triage,
regressions, CI checks, and AI-agent debugging.

```console
$ rwave search sim.fst --condition 'valid=1,ready=1'
Found: 3 interval(s)
  30ns        ..50ns         valid=1,ready=1
  70ns        ..80ns         valid=1,ready=1
  90ns        ..110ns        valid=1,ready=1
```

## Features

- **Reads what your simulator wrote.** VCD, FST, and GHW natively; WLF
  (Mentor/Questa) and FSDB (Synopsys/Verdi) on linux-amd64 — experimental, see
  [below](#experimental-wlf--fsdb).
- **Seven focused commands.** Overview, signal list, value-change dump,
  per-signal stats, point snapshot, two-point diff, and conditional search —
  no GUI, no modes to learn.
- **JSON on everything.** `--json` gives every command a stable, machine-readable
  form; time is reported as both raw ticks and human units (`460ns`). Built for
  agents and CI.
- **Scales to big dumps.** Streaming, memory-bounded commands and an `O(n log k)`
  k-way-merge replay handle hundreds of thousands of signals without exhausting
  RAM.
- **One binary, nothing to install.** Pure Rust, no Python, no GUI toolkit, no
  runtime dependencies for the core.

## Installation

### Prebuilt binaries

Download the binary for your platform from the
[latest release](https://github.com/neveltyc/RWaveAnalyzer/releases/latest):

| Platform | Binary | VCD · FST · GHW | WLF | FSDB |
|---|---|:---:|:---:|:---:|
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

Every binary reads VCD/FST/GHW. `rwave-linux-amd64` is glibc-dynamic
(glibc ≥ 2.17 / manylinux2014), so it runs on any mainstream Linux since 2014.

### From source

```sh
cargo build --release      # → target/release/rwave
```

A recent stable Rust toolchain is all you need (built against 1.90, edition
2024). The parser front-end is vendored, so the build is offline and pins an
exact parser revision. To cross-compile the release binaries, see
[docs/BUILD.md](docs/BUILD.md).

## Quick start

Point any command at a `.vcd`, `.fst`, or `.ghw` file. Start with `info` for the
lay of the land:

```console
$ rwave info sim.fst
File      : sim.fst
Size      : 4198400 bytes
Timescale : 1ps
Signals   : 1043
Types     : wire=712, reg=301, parameter=30
Time      : 0s ~ 1.2ms (1.2ms)
  scope: tb
  scope: tb.dut
```

Then drill in — list signals, dump a window, find when something happens:

```sh
rwave list     sim.fst --filter clk,rst              # which signals exist?
rwave dump     sim.fst --begin 10us --end 12us --filter cpu.state
rwave summary  sim.fst --filter alu                  # which signals are active?
rwave snapshot sim.fst --at 17.5us                   # all values at one instant
rwave compare  sim.fst --at 17.5us,17.7us            # what changed between two times?
rwave search   sim.fst --condition 'valid=1,ready=1' # when does a condition hold?
```

Add `--json` to any command for a stable, scriptable form — this is what an
agent or CI job consumes (abbreviated; every text field has a JSON counterpart):

```console
$ rwave --json info sim.fst
{"file":"sim.fst","timescale":"1ps","signal_count":1043,
 "var_types":{"wire":712,"reg":301,"parameter":30},
 "time_min_ticks":0,"time_max_ticks":1200000000,"duration_h":"1.2ms",
 "scopes":["tb","tb.dut"]}
```

## Usage

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
```

| Command | What it answers |
|---|---|
| `info`     | What's in this file? — timescale, signal/type counts, time span, scopes |
| `list`     | Which signals exist? — paths + bit widths (`--filter` matches any alias) |
| `dump`     | What happened between two times? — value-change events in time order |
| `summary`  | Which signals are active vs static? — per-signal change/edge counts |
| `snapshot` | What is everything at time T? — `--at T` |
| `compare`  | What changed between two times? — `--at T1,T2` |
| `search`   | When does a condition hold? — `--condition`, with `--show` / `--changed` |

**Global options:** `--json` (structured output) · `--limit N` (max rows;
default 200, `0` = unlimited) · `--verbose` (extra fields; also lifts truncation
when `--limit` is omitted).

- **Times** accept `fs/ps/ns/us/ms/s` suffixes (e.g. `17.5us`); a bare integer
  is raw ticks.
- **Filters** are comma-separated and match by substring or `*` glob (e.g.
  `--filter '*valid,top.dma.*'`).
- **Conditions** (search only) are a comma-separated AND-list of `SIG=VAL` /
  `SIG!=VAL`, with values in decimal, hex (`0xff`), binary (`b1010`), or 4-state.

For the full surface — every flag, the search sub-modes, value formatting — run
`rwave <command> --help`.

## Experimental: WLF & FSDB

On **linux-amd64**, rwave additionally reads two proprietary vendor formats:

| Format | Vendor | Read via | Point rwave at it with |
|---|---|---|---|
| `.wlf`  | Mentor/Siemens Questa, ModelSim | `libwlf`  | `RWAVE_WLF_LIB`  → `libwlf.so`  |
| `.fsdb` | Synopsys Verdi                  | Verdi NPI | `RWAVE_FSDB_LIB` → `libNPI.so`  |

Set the env var to the vendor library from your own licensed install, then use
rwave exactly as for any other format:

```sh
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
rwave info sim.fsdb
```

These backends `dlopen` the vendor library at runtime, so the vendor's simulator
must be installed with a **valid license** (FSDB additionally requires a
**Verdi-Ultra** license feature). For FSDB, source your Verdi environment first
so `libNPI.so` can resolve `$VERDI_HOME` and its own dependencies. Licensing and
environment setup are the vendor's domain.

> [!IMPORTANT]
> **WLF/FSDB support is experimental and linux-amd64 only.** rwave reads these
> formats *only* through each vendor's own public reader interface. It bundles
> **no proprietary binaries and no vendor source code**, links none at build
> time, and redistributes no vendor software — at runtime it `dlopen`s the
> library *you* supply from *your* licensed install. Using these formats
> therefore requires the vendor's software and license on your machine;
> obtaining and configuring those, under the vendor's terms, is your
> responsibility.

Need a different reader for a format, or support for a brand-new one? rwave loads
any backend that implements its small C ABI from `$RWAVE_PLUGIN_<EXT>` — see
[docs/PLUGIN.md](docs/PLUGIN.md).

## For AI agents

The repo ships an agent skill at [skill/SKILL.md](skill/SKILL.md): a decision
tree from user intent to command, a JSON-field cheat sheet, the condition
grammar, and a handful of debugging workflows. Point your agent at it and let
the `--json` output of every command do the rest.

## Documentation

- [docs/BUILD.md](docs/BUILD.md) — building and cross-compiling the release binaries
- [docs/PLUGIN.md](docs/PLUGIN.md) — the C-ABI backend interface for custom formats
- [docs/DESIGN.md](docs/DESIGN.md) — architecture, performance, and compatibility notes
- [skill/SKILL.md](skill/SKILL.md) — the AI-agent skill

## License

MIT — see [LICENSE](LICENSE). Vendored components keep their own licenses:
`vendor/wellen` and `vendor/fst-reader` are both BSD-3-Clause.
