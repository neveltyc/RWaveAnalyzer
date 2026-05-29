# RWaveAnalyzer (`rwave`)

An AI-agent-friendly, debug-oriented waveform analyzer for RTL simulation
dumps. `rwave` reads **VCD** and **FST** (and GHW) files and exposes a small,
scriptable command set — file overview, signal listing, value-change dumps,
per-signal statistics, point/pair snapshots, and conditional search — with both
human-readable text and compact JSON output.

The command surface intentionally mirrors the reference Python tool
[`VCD_ANALYZER`](https://github.com/neveltyc/VCD_ANALYZER) so the two are
drop-in compatible at the CLI, while `rwave` additionally understands FST and is
built for large dumps.

## Why another analyzer

* **Format-neutral core.** VCD is just the first format. The parser is isolated
  behind a backend trait, so adding a new waveform format (or a faster reader
  for an existing one) does not touch the command set, formatting, or search.
* **Built for scale.** Value-change replay is a binary-heap k-way merge
  (`O(n log k)`); whole-file commands stream their work in memory-bounded
  batches so multi-hundred-thousand-signal dumps don't exhaust RAM.
* **Agent-friendly output.** Every command has a `--json` form with stable keys,
  and time is reported both as raw ticks and human units.

## Build

Requires a recent stable Rust toolchain (developed against 1.90, edition 2024).

```sh
cargo build --release
# binary: target/release/rwave
```

The parser front-end (`wellen`) and its FST dependency (`fst-reader`) are
**vendored** under `vendor/` — the build needs no network and pins the exact
parser revision. `vendor/fst-reader` additionally carries a local fix for an
upstream out-of-bounds crash on FSTs with sparse/aliased signal handles (such as
VCS output); see `CHANGELOG.md`.

### Release binaries (static Linux + Windows)

```sh
rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-gnu
# Debian/Ubuntu host packages for cross-linking:
sudo apt-get install -y musl-tools gcc-mingw-w64-x86-64

./scripts/build-release.sh
# dist/rwave-linux-amd64        (fully static, musl)
# dist/rwave-windows-amd64.exe  (MinGW-w64)
```

Cross-linker selection lives in `.cargo/config.toml`.

## Usage

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
```

| Command    | Purpose                                                        |
|------------|----------------------------------------------------------------|
| `info`     | File overview: timescale, signal/type counts, time span, scopes |
| `list`     | Signal paths with bit widths (`--filter` to narrow)             |
| `dump`     | Value-change events in time order (`--begin/--end/--filter`)    |
| `summary`  | Per-signal stats: change count, rise/fall edges, static detection |
| `snapshot` | Known signal values at one time point (`--at T`)                |
| `compare`  | Value diff between two time points (`--at T1,T2`)               |
| `search`   | Conditional search with associated-signal observation           |

Global options: `--json` (structured output), `--limit N` (max rows; default
200, `0` = unlimited), `--verbose` (extra fields; also lifts truncation when
`--limit` is omitted).

Times accept `fs/ps/ns/us/ms/s` suffixes (e.g. `17.5us`); a bare integer is raw
ticks. Unit-suffixed times are scaled by the file's timescale using
banker's rounding, matching the reference tool.

### Examples

```sh
rwave info design.fst
rwave list design.vcd --filter clk,rst
rwave --json dump design.fst --begin 10us --end 12us --filter cpu.state
rwave summary design.vcd --filter alu
rwave snapshot design.fst --at 17.5us
rwave compare design.fst --at 17.5us,17.7us --filter bus
rwave search design.vcd --condition 'valid=1,ready=1' --show data --changed data
```

## Architecture

The crate is layered top-to-bottom, each layer depending only on those below:

```
        cli            argument parsing only
         │
      commands         per-command logic + presentation (text/JSON)
         │
       model           format-neutral domain: signal table, replay, snapshots
         │
      backend          WaveformBackend trait (the parser contract)
         │
   backend::wellen_backend   the only code that touches `wellen`
```

Leaf utilities used by `commands` but coupled to nothing below them:
`format` (value/time formatting and parsing), `filter` (signal pattern
matching), `condition` (search predicates), and `json` (a compact serializer).

The key boundary is **`WaveformBackend`**. It hands the model fully decoded,
owned per-signal traces (parallel time/value arrays); the model owns all replay,
merging, and snapshot logic over plain slices. Because the trait surface is
coarse (no per-sample virtual call), the hot path stays monomorphic, and
swapping parsers means adding one file under `backend/`.

```
crates/rwave/src/
  cli.rs          argument grammar, help text, validation
  commands.rs     the seven commands, text + JSON emitters
  model.rs        Wave: signal table, heap-merge replay, bounded streaming
  backend/
    mod.rs        WaveformBackend trait + neutral types
    wellen_backend.rs   wellen adapter (only wellen-aware module)
  format.rs       fmt_val, time parse/format, Python-repr quoting
  filter.rs       glob-lite + substring signal matching
  condition.rs    search condition parsing/evaluation, big-uint compare
  json.rs         compact JSON builder (matches Python json.dumps separators)
  main.rs         argv → Wave::open → dispatch, exit codes, SIGPIPE
vendor/
  wellen/         vendored parser front-end
  fst-reader/     vendored + locally patched FST reader
verify/
  stimulus_src/   Verilog testbenches
  stimulus/       generated matched VCD+FST pairs
  run.sh          self-test harness (smoke + VCD/FST parity)
scripts/
  build-release.sh, gen-stimulus.sh
```

## Performance notes

* **Replay**: a binary min-heap k-way merge over selected signals' traces,
  `O(n log k)` for `n` changes across `k` signals; ties within a timestamp
  resolve to writer (declaration) order.
* **Snapshots / compare**: per-signal binary search for the last value at or
  before the target time — no full replay.
* **Whole-file commands** (`summary`, and unfiltered `dump`/`snapshot`/`compare`)
  decode signals in memory-bounded batches and release each batch, so peak
  memory is proportional to a batch rather than the whole file. `summary`
  computes its per-signal-independent statistics directly from each trace with
  an allocation-light inner loop. `dump` retains only the earliest `--limit`
  events via a bounded heap.

These paths produce byte-identical output to the simple eager paths; the switch
is purely a memory/throughput optimization keyed on selection size.

## Testing

```sh
cargo test            # unit tests (formatting, filters, conditions, CLI parsing)
bash verify/run.sh    # smoke + VCD/FST parity on the bundled stimulus
```

`verify/run.sh` requires only the built binary. It checks that every command
runs on both a VCD and an FST, and that the value-bearing commands produce
identical results across formats for the same design.

## Known differences from the reference tool

`rwave` reproduces the `VCD_ANALYZER` CLI output, with a few principled
exceptions that stem from using a real parser front-end and from generalizing
beyond VCD:

* **Format-neutral wording.** Messages that named "VCD" specifically are
  generalized (e.g. `cannot open waveform file`), since `rwave` handles
  multiple formats.
* **`list --verbose` identifier field.** The reference prints the raw VCD
  identifier code (`!`, `$`, …). `rwave` reports the backend's signal index
  instead, because the abstract backend does not expose VCD identifier codes.
* **Comments / synthesized buses.** `wellen`'s reader does not preserve VCD
  `$comment` blocks, so `comments` is empty; `synthesized_buses` is reported as
  0 (no backend equivalent).
* **FST conversion artifact (tooling, not `rwave`).** `vcd2fst` does not carry
  Verilog `parameter`/`localparam` *values* into the FST. A design that declares
  such constants will show them with values in its VCD but without values in the
  converted FST; `rwave` faithfully reports whatever each file actually contains.
* **`dump` event order within a single timestamp.** When several signals change
  at the *same* time, the reference emits them in the order their value-changes
  physically appear in the VCD (which depends on the writer — Icarus Verilog,
  for instance, emits its initial `$dumpvars` block in reverse-declaration
  order). `wellen` stores changes per-signal and does not preserve that
  cross-signal file order, so `rwave` orders simultaneous events by declaration
  order instead. **All values, timestamps, and the set of emitted events are
  identical** — only the relative order of events sharing a timestamp can
  differ, and only for `dump`. (The reference tool is VCD-only and cannot read
  FST at all, so there is no cross-format reference for FST ordering.)

## License

MIT. See `LICENSE`. Vendored components retain their own licenses:
`vendor/wellen` (BSD-3-Clause) and `vendor/fst-reader` (BSD-3-Clause).
