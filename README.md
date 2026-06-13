<p align="center">
  <h1 align="center">RWaveAnalyzer</h1>
  <p align="center">
    A fast, single-binary CLI for inspecting RTL simulation waveforms &mdash;
    <b>VCD</b>, <b>FST</b>, <b>GHW</b>, and even the proprietary <b>WLF</b> &amp; <b>FSDB</b> &mdash;
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

You have a multi-gigabyte FST from an overnight regression and you need to know
exactly when `arvalid` and `arready` were both high — or what `state[3:0]` held
at 17.55 µs. Opening Verdi or GTKWave means waiting for a GUI, clicking down the
hierarchy, and squinting at a cursor. `rwave` answers from the terminal, in one
command:

```sh
rwave search sim.fst --condition 'arvalid=1,arready=1' --show araddr,arlen
```

Two things set it apart:

- **It reads every format your flow produces.** The open **VCD**, **FST**, and
  **GHW** — *and* the proprietary **WLF** (Mentor/Questa) and **FSDB**
  (Synopsys/Verdi) dumps, the latter two read directly through each vendor's own
  library with no conversion step. See [WLF & FSDB](#wlf--fsdb).
- **Every command speaks JSON.** A `--json` mode with stable keys turns the same
  tool into a backend for CI gates and AI agents, not just a human at a prompt.

It is a single self-contained binary — pure Rust, no Python, no GUI, nothing to
install — and it streams its work so a dump with hundreds of thousands of
signals never exhausts RAM.

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

# Which signals are active vs static?
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
[WLF & FSDB](#wlf--fsdb)). `rwave-linux-amd64` is glibc-dynamic (glibc ≥ 2.17),
so it runs on any mainstream Linux since 2014.

**From source** — all you need is a recent stable Rust toolchain (built against
1.90):

```sh
cargo build --release      # → target/release/rwave
```

The parser is vendored, so the build is offline and reproducible; see
[docs/BUILD.md](docs/BUILD.md) for cross-compiling the release binaries.

## Commands

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
```

| Command | What it does |
|:--|:--|
| `info`     | Timescale, signal/type counts, time span, scopes — the file at a glance |
| `list`     | Enumerate signals with path, width, and type (`--filter` matches any alias) |
| `dump`     | Print every value change in a time window, in order |
| `summary`  | Per-signal stats: active/static, change count, rise/fall edges |
| `snapshot` | All known signal values at one time point (`--at T`) |
| `compare`  | What changed between two times (`--at T1,T2`) |
| `search`   | Find intervals where a condition holds, optionally watching related signals |

All commands take `--begin`/`--end` windows with unit suffixes
(`fs ps ns us ms s`; a bare integer is raw ticks) and `--filter` with substring
or `*`-glob patterns. Global flags: `--json` (structured output), `--limit N`
(max rows; default 200, `0` = unlimited), `--verbose` (extra fields). A search
condition is a comma-separated AND-list of `SIG=VAL` / `SIG!=VAL`, with values in
decimal, hex (`0xff`), binary (`b1010`), or 4-state. Run `rwave <command> --help`
for the full reference.

## JSON output

Under `--json` every command emits compact structured JSON with raw tick counts
(`*_ticks`) alongside human-readable times (`*_h`) — built for agents, scripts,
and CI gates rather than eyeballs:

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition 'state=5' --show data
```

## WLF & FSDB

Beyond the open formats, **rwave reads the two dominant proprietary EDA dumps
directly** — no `wlf2vcd` / `fsdb2vcd` step, no intermediate file:

| Format | Vendor | Read through | Point rwave at it with |
|:--|:--|:--|:--|
| `.wlf`  | Mentor/Siemens Questa, ModelSim | `libwlf`  | `RWAVE_WLF_LIB`  → `libwlf.so`  |
| `.fsdb` | Synopsys Verdi                  | Verdi NPI | `RWAVE_FSDB_LIB` → `libNPI.so`  |

rwave loads the reader library from *your own* licensed tool install at runtime,
so there is nothing proprietary to ship. Set the env var and use rwave exactly as
for any other format:

```sh
# WLF — point at libwlf.so from your Questa / ModelSim install
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf

# FSDB — point at libNPI.so from your Verdi install
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
rwave info sim.fsdb
```

This support is **experimental and linux-amd64 only**. The vendor's simulator
must be installed with a valid license (FSDB additionally needs a **Verdi-Ultra**
feature); for FSDB, source your Verdi environment first so `libNPI.so` can
resolve `$VERDI_HOME` and its own dependencies. Need a different reader for a
format, or support for a brand-new one? rwave loads any backend implementing its
small C ABI from `$RWAVE_PLUGIN_<EXT>` — see [docs/PLUGIN.md](docs/PLUGIN.md).

> Disclaimer — rwave reads WLF and FSDB only through each vendor's own public
> reader interface. It bundles no proprietary binaries and no vendor source code,
> links none at build time, and redistributes no vendor software; at runtime it
> `dlopen`s the library you supply from your own licensed install. Using these
> formats requires the vendor's software and license on your machine, and
> obtaining and configuring those under the vendor's terms is your
> responsibility.

## For AI agents

The repo ships an agent skill at [skill/SKILL.md](skill/SKILL.md): a decision
tree from user intent to command, a JSON-field cheat sheet, the condition
grammar, the WLF/FSDB setup, and a handful of debugging workflows. Point your
agent at it and let the `--json` output of every command do the rest.

## Documentation

- [docs/BUILD.md](docs/BUILD.md) — building and cross-compiling the release binaries
- [docs/PLUGIN.md](docs/PLUGIN.md) — the C-ABI backend interface for custom formats
- [docs/DESIGN.md](docs/DESIGN.md) — architecture, performance, and compatibility notes
- [skill/SKILL.md](skill/SKILL.md) — the AI-agent skill

## License

MIT — see [LICENSE](LICENSE). Vendored components keep their own licenses:
`vendor/wellen` and `vendor/fst-reader` are both BSD-3-Clause.
