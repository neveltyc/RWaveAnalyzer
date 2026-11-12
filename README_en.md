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

<p align="center">
  <b>English</b> · <a href="README.md">中文</a>
</p>

---

Find when `arvalid` and `arready` were both high with one command, no GUI:

```sh
rwave search sim.fst --condition "arvalid=1,arready=1" --show araddr,arlen
```

- **Single binary**, no runtime dependencies.
- **Formats**: VCD, FST, GHW; experimental WLF and FSDB on linux-amd64.
- **JSON output**: every command takes `--json`, with stable keys for scripts, CI, and AI agents.
- **Large files**: whole-file commands stream in batches with bounded memory.

## Quick start

```sh
# What's in this file?
rwave info sim.fst

# Show me the clock and reset
rwave list sim.fst --filter '*clk*,*rst*'

# What happened between 100 ns and 200 ns?
rwave dump sim.fst --begin 100ns --end 200ns --filter state

# When were valid and ready both high?
rwave search sim.fst --condition "valid=1,ready=1" --show data

# At which instants does req toggle while ready is low?
rwave search sim.fst --condition "changed(req),ready=0" --show state

# When does any channel handshake? (repeat --condition to OR)
rwave search sim.fst --condition "ch0_valid=1,ch0_ready=1" --condition "ch1_valid=1,ch1_ready=1"

# Values at 17.55 us
rwave snapshot sim.fst --at 17.55us --filter state,init_done

# What changed between two times?
rwave compare sim.fst --at 17.5us,17.7us --filter bus

# Which signals are active versus static?
rwave summary sim.fst --filter alu

# Just this block, not its submodules
rwave list sim.fst --scope u_tx.u_fifo --depth 1

# Everything but the CDC synchronizers
rwave summary sim.fst --exclude '*_sync_*.*'
```

## Install

Download the binary for your platform from the [latest release](https://github.com/neveltyc/RWaveAnalyzer/releases/latest):

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

`rwave-linux-amd64` needs glibc 2.17 or newer.

## Building from source

Needs a stable Rust toolchain, 1.90 or newer. No other system dependencies.

```sh
cargo build --release                         # → target/release/rwave
cargo build --release --no-default-features   # VCD/FST/GHW only
```

The WLF and FSDB backends compile only on x86_64 Linux; other platforms get VCD/FST/GHW.

Cross-building the release binaries uses [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild); the macOS target builds only on macOS:

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
./scripts/build-release.sh --target linux-amd64   # one target
```

Details in [docs/BUILD.md](docs/BUILD.md). Tests:

```sh
cargo test
bash verify/run.sh
```

## Commands

```
rwave [--json] [--limit N] [--verbose] [-o FILE] <command> <file> [options]
rwave --batch [--json] <file> [global-opts] < commands.txt
```

| Command | What it does |
|:--|:--|
| `info`     | Timescale, signal counts, time span, scopes |
| `list`     | Signal paths, widths, types |
| `dump`     | Every value change in a time window |
| `summary`  | Per-signal change count, rising/falling edges, unknown bits seen |
| `snapshot` | All signal values at one time |
| `compare`  | What changed between two times |
| `search`   | Spans where a condition holds, or instants a `changed()` fires |
| `tree`     | Browse the hierarchy: a scope's children, or `--of SIGNAL`'s ancestors |
| `trace`    | A signal's drivers and loads with `file:line` (FSDB only, see [trace](#tracing-drivers-and-loads)) |

| Option | What it does |
|:--|:--|
| `--json`            | Compact JSON, see [JSON output](#json-output) |
| `--limit N`         | At most N rows, default 500 (unlimited with `-o` or `--verbose`), `0` = unlimited; a cut ends with a hint line |
| `--verbose`         | More fields |
| `-o, --output FILE` | Full result to FILE (created only on success); stdout gets a one-line receipt |
| `--begin T` / `--end T` | Time window for `dump`, `summary`, `search` |
| `--at T`            | Time point for `snapshot`; `compare` takes `--at T1,T2` |

- Times take a `fs`, `ps`, `ns`, `us`, `ms`, or `s` suffix (`17.5us`); a bare number is raw ticks.
- Terminal output stops at 1 MiB with exit 3; pipes and files are unlimited.
- `rwave --help` is the syntax summary; `rwave --help --verbose` prints the full manual.

### Search conditions

| Syntax | Meaning |
|:--|:--|
| `SIG=VAL` / `SIG!=VAL` | Equal / not equal |
| `changed(SIG)`         | Instants SIG toggles, reported as times instead of spans |
| `--show K1,K2`         | Spans split wherever a shown value changes, so the row count depends on `--show` |
| `A=1,B=0`              | Comma = AND |
| Repeated `--condition` | OR between them; all use `changed()` or none do |

| Value | Syntax |
|:--|:--|
| Decimal  | `cnt=5`; negatives are two's complement at the signal width, `cnt=-1` |
| Hex      | `addr=0xff` |
| Binary / 4-state | `state=b1010`, `bus=bx`; `?` is a don't-care, `status=b?????1??` tests bit 2 (short literals zero-extend, so write every bit) |
| real     | Numeric compare, `temp=-1.5` |
| string   | A number or bit literal, compared as text, `s=5` |

A value the signal can never hold is an error, e.g. `state=99` on a 4-bit `state`.

## Selecting signals

Every command except `info`, `tree`, and `trace` takes these five options (`tree` takes `--scope` and `--depth` only):

| Option | What it does |
|:--|:--|
| `--scope P1,P2`   | Only these subtrees; segment-aligned, so `u_fifo` never hits `u_fifo_ctrl` |
| `--depth N`       | Levels below the scope, direct signals = 1; needs `--scope` (except `tree`) |
| `--filter K1,K2`  | Keep matches |
| `--exclude K1,K2` | Drop matches, applied last |
| `--exact`         | Full names only, no substring fallback |

Matching rules:

- A pattern without `.` or `/` matches the signal name; with one, the full path, e.g. `--filter 'u_dma.'`. The two separators are interchangeable.
- If a signal's full name equals the pattern, only that signal is selected: `top.sig3` does not select `top.sig30`. Paths may start at any segment (`u_dma.req`); the bus range is optional (`state` selects `state[2:0]`).
- With no exact name, the pattern matches as a substring; write `name*` for a prefix.
- `*` and `?` make an anchored glob, bus range optional (`*data` finds `data[7:0]`); `[` and `]` are literal; case-insensitive; commas mean OR.
- `--exclude` and `search`'s `--condition`/`--show` follow the same rules: `--exclude clk` drops only `clk`; drop every clock with `--exclude '*clk*'`.
- No match is an empty result, not an error; `--filter ''` is the same as omitting it.

## JSON output

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition "state=5" --show data
```

- Every time comes as raw ticks (`*_ticks`) and human-readable (`*_h`).
- For a given command and flags, the key set and order are fixed; empty values are `null`, `0`, `[]`, or `{}`.
- Errors are JSON on stderr: `{"command":"dump","ok":false,"error":"..."}`.

| Value type | Format |
|:--|:--|
| 1-bit logic | `0`, `1` |
| Multi-bit   | `0x<hex>`, no zero padding, e.g. `0x4` |
| Unknown bits | Full-width `b<bits>`, e.g. `bx`, `b01x0`; test the `b` prefix, not for an `x` |
| real        | Shortest exact decimal, e.g. `1.5`, `NaN` |
| string      | As is |

| Exit | Meaning |
|:--|:--|
| 0 | Answered, empty results included (`hint` says why) |
| 1 | File or environment: unreadable or unparsable file, design library or license unavailable, result not written |
| 2 | Bad request: wrong option, signal name, condition, or time; fix the command |
| 3 | Terminal output cut at 1 MiB; write to a file or pipe instead |

Per-command fields: [docs/JSON.md](docs/JSON.md).

## Batch mode

`--batch` loads the file once and runs the commands on stdin in order, saving the load time on repeated queries against one large file (FSDB and WLF especially).

```sh
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'dump --begin 1us --end 2us --filter state' \
  'search --condition valid=1,ready=1 --show data' \
  | rwave --batch --json sim.fst
```

```
{"id":"1","ok":true,"result":{ …info… }}
{"id":"2","ok":true,"result":{ …list… }}
```

- One command per line, without `rwave` and the file. Blank and `#` lines are skipped; a trailing `#label` names the result.
- Options on the `--batch` command line are per-line defaults; a line can override them, and an empty value (`--filter ''`) drops one.
- Results come back in input order, identical to running each command alone: one NDJSON line each with `--json`, a `#label` header each in text mode.
- A failed command reports `"ok":false` and its standalone `exit`, and the batch continues.

## WLF and FSDB (experimental)

linux-amd64 only. rwave reads them in place through the vendor's own libraries, no conversion.

### WLF

```sh
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf
```

### FSDB

Two ways:

| | Built-in NPI backend | [rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin) |
|:--|:--|:--|
| Get it | Included in `rwave-linux-amd64` | Build it on a machine with Verdi |
| License | Verdi-Ultra | No Verdi-Ultra needed |
| Set | `VERDI_HOME` | `RWAVE_PLUGIN_FSDB` |
| `trace` | ✓ | — |

Built-in backend:

```sh
export VERDI_HOME=/path/to/verdi     # usually set by sourcing the Verdi env
rwave info sim.fsdb
```

Plugin:

```sh
# build on a machine with Verdi
git clone https://github.com/neveltyc/rwave-open-fsdb-plugin
cd rwave-open-fsdb-plugin
./configure && make bundle

# deploy
mkdir -p ~/.rwave
tar xzf dist/rwave_fsdb_backend-*-linux_x86_64.tar.gz -C ~/.rwave --strip-components=1
export RWAVE_PLUGIN_FSDB="$HOME/.rwave/librwave_fsdb_backend.so"
rwave info sim.fsdb
```

With `RWAVE_PLUGIN_FSDB` set, the plugin overrides the built-in backend. The plugin must match rwave's backend ABI version (currently v2); on a mismatch rwave errors out, and rebuilding the plugin fixes it.

### Tracing drivers and loads

`trace` finds a signal's drivers or loads, with the source statement and `file:line`. It needs an FSDB opened through the built-in NPI backend and checks out a license while it runs.

```bash
rwave trace sim.fsdb tb.dut.u_core.u_alu.res
rwave trace sim.fsdb tb.dut.u_core.u_alu.res --load --at 1250ns
```

It needs the Verdi design library, built with `vcs -kdb -debug_access+all`, or `vericom -kdb` plus `elabcom -elab kdb`. rwave reads its path from the FSDB header; if that path has moved, pass `--kdb`.

| Option | What it does |
|:--|:--|
| `--load`     | Who reads the signal instead of who drives it |
| `--at T`     | Annotate each endpoint with its value at T |
| `--control`  | Include clocks, resets, and enclosing `if`/`case` dependencies |
| `--kdb DIR`  | Design library path |
| `--top NAME` | Design top module, when it differs from the waveform's |

### Environment variables

| Variable | Required | What it does |
|:--|:--|:--|
| `VERDI_HOME`        | for `.fsdb` | Verdi install dir; rwave finds `libNPI.so` and `libnpiL1.so` under it |
| `RWAVE_WLF_LIB`     | for `.wlf`  | Path to `libwlf.so` |
| `RWAVE_TRACE_EN`    | optional | `0` disables `trace` |
| `RWAVE_FSDB_LIB`    | optional | Path to `libNPI.so`, for a non-standard Verdi layout |
| `RWAVE_NPI_L1_LIB`  | optional | Path to `libnpiL1.so`, when it isn't next to `libNPI.so` |
| `RWAVE_PLUGIN_FSDB` | optional | Path to the FSDB plugin's `librwave_fsdb_backend.so` |

Other formats can load your own backend via `$RWAVE_PLUGIN_<EXT>`; see [docs/PLUGIN.md](docs/PLUGIN.md).

## Disclaimer

rwave contains, links, and distributes no vendor software; at runtime it loads libraries from your own licensed installation. Obtaining and using that software and its licenses under the vendor's terms is your responsibility.

## For AI agents

[skill/SKILL.md](skill/SKILL.md) is the agent skill and the full manual that `rwave --help --verbose` prints: principles, command choice, a JSON field reference, and debug recipes.

## License

MIT, see [LICENSE](LICENSE). `vendor/wellen` and `vendor/fst-reader` are BSD-3-Clause. Notices for the dependencies in the release binaries: [third-party-licenses.md](third-party-licenses.md).
