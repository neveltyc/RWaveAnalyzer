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
which it reads through each vendor's own library (see [WLF and FSDB](#experimental-support-for-wlf-and-fsdb)).
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
[WLF and FSDB](#experimental-support-for-wlf-and-fsdb)). The `rwave-linux-amd64` build is dynamically linked
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
rwave --batch [--json] <file> [global-opts] < commands.txt
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
agent rather than only by a person reading the terminal. Signal values render
compactly for the same reason: a 1-bit logic signal as `0`/`1`/`x`/`z`, a
multi-bit bus as `0x<hex>` with leading zeros stripped (e.g. `0x4`), a bus with
unknown bits as `b<bits>` (e.g. `b01x0`), and real/string values verbatim. The
width is in each signal's metadata, so it is not re-encoded as hex padding.

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition 'state=5' --show data
```

## Batch mode

Large FSDB and WLF databases are read through a vendor library, where each
"open" spins up a C++ runtime and indexes the whole hierarchy — seconds to tens
of seconds for a multi-gigabyte file. When several queries target the *same*
file (a CI gate, a scripted extraction, an AI agent's multi-step plan), paying
that cost once instead of once per query matters. `--batch` does exactly that:
it loads the file **once**, then runs a list of commands read from stdin.

```sh
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'dump --begin 1us --end 2us --filter state' \
  'search --condition valid=1,ready=1 --show data' \
  | rwave --batch --json sim.fst
```

Each input line is an ordinary command with the leading `rwave` and the file
omitted — both are already fixed by the `--batch` invocation. Blank lines and
lines beginning with `#` are skipped; a trailing `#label` names that line's
result. Any `[global-opts]` on the `--batch` command line (`--limit`,
`--verbose`, …) become defaults that an individual line can override.

Results come back in input order, one per command. With `--json` each is a
single NDJSON object; without it, each is a `#label` header followed by that
command's usual text output:

```
{"id":"1","ok":true,"result":{ …info… }}
{"id":"2","ok":true,"result":{ …list… }}
```

A batch `result` is identical to what the equivalent single command would
produce — batch only saves the repeated load, it never changes a command's
output. A command that fails (an unknown signal, an illegal time) is reported
with `"ok":false` and does **not** stop the batch; the run still exits `0`. Only
a file that cannot be loaded, or a command stream that cannot be read, is fatal.

## Experimental support for WLF and FSDB

On linux-amd64, RWaveAnalyzer provides experimental support for two vendor
waveform databases — Mentor/Siemens **WLF** and Synopsys **FSDB** — by calling
into each vendor's own reader library at runtime. There is no format conversion
step and no intermediate file.

### WLF

rwave reads Questa / ModelSim `.wlf` files through `libwlf.so`. Point
`RWAVE_WLF_LIB` at the library from your Questa installation:

```sh
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf
```

The vendor tool must be installed on the same machine; rwave loads `libwlf.so`
at runtime and does not ship it.

### FSDB

rwave supports two ways to read `.fsdb` files. Both are experimental and
linux-amd64 only.

**Built-in backend (NPI)** — ships with the `rwave-linux-amd64` binary; no
extra build step. rwave calls Synopsys's NPI (Novas Programming Interface)
through `libNPI.so` from your Verdi installation. This path requires a
Verdi-Ultra license feature on the host:

```sh
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
rwave info sim.fsdb
```

Source your Verdi environment first so that `libNPI.so` can locate `$VERDI_HOME`
and its dependent libraries.

**Plugin backend
([rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin))**
— a source-only plugin that reads FSDB through Synopsys's FsdbReader interface.
You compile it yourself on a machine that has a licensed Verdi installation,
because the build links against vendor libraries that cannot be redistributed.
This path does not require the Verdi-Ultra license feature that the NPI backend
needs — if you need to read FSDB on any linux-amd64 environment, choose this
approach:

```sh
# build on a machine with Verdi
git clone https://github.com/neveltyc/rwave-open-fsdb-plugin
cd rwave-open-fsdb-plugin
./configure && make bundle

# deploy — unpack the bundle, point rwave at the plugin
mkdir -p ~/.rwave
tar xzf dist/rwave_fsdb_backend-*-linux_x86_64.tar.gz -C ~/.rwave --strip-components=1
export RWAVE_PLUGIN_FSDB="$HOME/.rwave/librwave_fsdb_backend.so"
rwave info sim.fsdb
```

When `RWAVE_PLUGIN_FSDB` is set it overrides the built-in NPI backend for
`.fsdb` files.

### Environment variables

| Variable | What it does |
|:--|:--|
| `RWAVE_WLF_LIB`    | Absolute path to `libwlf.so`. Enables built-in WLF reading. |
| `RWAVE_FSDB_LIB`   | Absolute path to `libNPI.so`. Enables built-in FSDB reading (NPI, needs Verdi-Ultra license). |
| `RWAVE_PLUGIN_FSDB` | Absolute path to `librwave_fsdb_backend.so` from the plugin build. Overrides the built-in FSDB backend. |

For other formats or a custom backend implementation, rwave loads any shared
library that implements its C ABI from `$RWAVE_PLUGIN_<EXT>` — see
[docs/PLUGIN.md](docs/PLUGIN.md).

## Disclaimer

RWaveAnalyzer reads WLF and FSDB only through each vendor's own reader library
interface. It contains no vendor binaries and no vendor source code, links
against none of them at build time, and redistributes no vendor software; at
runtime it loads the reader library that you supply from your own licensed
installation. The
[rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin) is
likewise source-only and ships no vendor binaries — you compile it against your
own Verdi installation. Reading these formats requires the vendor's software and,
where applicable, a valid license on your machine; obtaining and using those
under the vendor's terms is your responsibility.

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
verify/            self-test harness with committed stimulus
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
```

`verify/run.sh` needs only the built binary: it confirms that every command runs
on both a VCD and an FST, and that the value-bearing commands produce identical
results across the two formats for the same design — a self-contained regression
net that needs no external reference.

## License

MIT — see [LICENSE](LICENSE). The vendored components keep their own licenses:
`vendor/wellen` and `vendor/fst-reader` are both BSD-3-Clause.
