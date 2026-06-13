# RWaveAnalyzer (`rwave`)

An AI-agent-friendly, debug-oriented waveform analyzer for RTL simulation
dumps. `rwave` reads **VCD**, **FST**, and **GHW** natively — plus **WLF**
(Mentor/Questa) and **FSDB** (Synopsys/Verdi) on the linux-amd64 build — and
exposes a small, scriptable command set: file overview, signal listing,
value-change dumps, per-signal statistics, point/pair snapshots, and
conditional search, with both human-readable text and compact JSON output.

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

## Install

Prebuilt binaries are attached to each tagged
[GitHub Release](https://github.com/neveltyc/RWaveAnalyzer/releases):

| Binary | Core (VCD/FST/GHW) | WLF | FSDB |
|---|:---:|:---:|:---:|
| `rwave-linux-amd64`        | ✓ | ✓ | ✓ |
| `rwave-windows-amd64.exe`  | ✓ | — | — |
| `rwave-linux-arm64`        | ✓ | — | — |
| `rwave-macos-arm64`        | ✓ | — | — |

Download the right one, mark it executable, and run:

```sh
chmod +x rwave-linux-amd64
./rwave-linux-amd64 info design.fst
```

The VCD/FST/GHW core works on every binary. The experimental WLF/FSDB
backends are **linux-amd64 only** and need the vendor's simulator + library
configured (see
[Vendor formats](#vendor-formats--plugins)). `rwave-linux-amd64` is
glibc-dynamic (manylinux2014, glibc ≥ 2.17), so it runs on every mainstream
Linux from 2014 on; Alpine/musl-only systems build the core from source.

## Build from source

Requires a recent stable Rust toolchain (developed against 1.90, edition 2024).

```sh
cargo build --release
# binary: target/release/rwave
```

This builds for the host. The WLF/FSDB backends (default features `wlf`,
`fsdb`) are target-gated to `x86_64` linux; on any other host they compile
out, leaving the VCD/FST/GHW core. `--no-default-features` forces the pure
core anywhere.

The parser front-end (`wellen`) and its FST dependency (`fst-reader`) are
**vendored** under `vendor/` — the build needs no network and pins the exact
parser revision. `vendor/fst-reader` additionally carries a local fix for an
upstream out-of-bounds crash on FSTs with sparse/aliased signal handles (such as
VCS output); see `CHANGELOG.md`.

### Release binaries

`scripts/build-release.sh` cross-builds the four release binaries via
`cargo-zigbuild` (Zig as cross-linker), so the same recipe works from any
host (a macOS host is needed only for the macOS target). Each target gets the
right feature set automatically — WLF/FSDB are target-gated — and
`linux-amd64` is glibc-dynamic with a pinned glibc 2.17 baseline.

| target | triple | output |
|---|---|---|
| `linux-amd64`   | `x86_64-unknown-linux-gnu`   | `dist/rwave-linux-amd64`       |
| `linux-arm64`   | `aarch64-unknown-linux-musl` | `dist/rwave-linux-arm64`       |
| `windows-amd64` | `x86_64-pc-windows-gnu`      | `dist/rwave-windows-amd64.exe` |
| `macos-arm64`   | `aarch64-apple-darwin`       | `dist/rwave-macos-arm64`       |

```sh
# one-time setup (macOS):
brew install rustup zig
export PATH="$(brew --prefix)/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
rustup default stable
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu aarch64-apple-darwin

./scripts/build-release.sh                        # all four
./scripts/build-release.sh --target linux-amd64   # one target
```

The script checks its prerequisites and prints exact install commands for
anything missing. See `docs/BUILD.md`; cross-linker configuration lives in
`.cargo/config.toml`.

## Usage

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
```

| Command    | Purpose                                                        |
|------------|----------------------------------------------------------------|
| `info`     | File overview: timescale, signal/type counts, time span, scopes |
| `list`     | Signal paths with bit widths (`--filter` matches any alias)             |
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

## Vendor formats & plugins

rwave resolves a file by its extension:

| extension          | backend                              | available in          |
|--------------------|--------------------------------------|-----------------------|
| `vcd` `fst` `ghw`  | native (`wellen`)                    | every build           |
| `wlf`              | **built-in** — Mentor `libwlf`       | linux amd64           |
| `fsdb`             | **built-in** — Synopsys Verdi NPI    | linux amd64           |
| anything else      | external plugin                      | `$RWAVE_PLUGIN_<EXT>`   |

These backends are compiled into the linux-amd64 binary. They `dlopen` the
vendor library at runtime, located via an env var — nothing proprietary is
bundled or linked:

| env var          | points at                                          |
|------------------|----------------------------------------------------|
| `RWAVE_WLF_LIB`  | `libwlf.so` (a Questa/ModelSim install)            |
| `RWAVE_FSDB_LIB` | `libNPI.so` (a licensed Verdi install)             |

```sh
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
rwave info sim.fsdb
```

**Requirements (on the machine reading WLF/FSDB).** The corresponding vendor
simulator must be installed with a **valid license**: Mentor/Siemens Questa
(or ModelSim) for WLF, Synopsys Verdi for FSDB — FSDB additionally needs a
**Verdi-Ultra** license feature. Licensing is the vendor's domain; follow
their documentation (rwave neither configures nor manages it). You also
supply the vendor `.so` and point rwave at it with the env vars above.

To use a different FSDB reader, set `$RWAVE_PLUGIN_FSDB` to an external
backend `.so` — an external plugin **overrides** the built-in of the same
extension.

> **Experimental — disclaimer.** WLF and FSDB support is experimental and
> **linux-amd64 only**. rwave reads these formats *only* through each EDA
> vendor's own public reader
> interface. It bundles **no proprietary binaries and no vendor source code**,
> links none of them at build time, and redistributes no vendor software — it
> `dlopen`s, at runtime, the library *you* supply from *your* licensed install.
> Using these formats therefore requires the vendor's software and license on
> your machine; obtaining and configuring those, under the vendor's terms, is
> your responsibility.

**External plugins.** Any other extension `<ext>` is served by a backend
cdylib whose absolute path you give in `$RWAVE_PLUGIN_<EXT>` (uppercased).
rwave `dlopen`s it and drives the C ABI in
[`crates/rwave/include/rwave_backend.h`](crates/rwave/include/rwave_backend.h) —
no search path, no registry: one env var, one `.so`. Memory ownership,
threading, and a conformance checklist live in
[`docs/PLUGIN.md`](docs/PLUGIN.md).

Diagnostics: `.wlf`/`.fsdb` on a build without that backend prints
`<fmt> support is only available in the linux-x86_64 build.`; an unhandled
extension prints `no backend for .<ext> files. Set RWAVE_PLUGIN_<EXT> ...`; an
external plugin built against a different `RWAVE_BACKEND_ABI_VERSION` is
rejected with a version-mismatch message.

## Agent skill

This repository includes [`skill/SKILL.md`](skill/SKILL.md) for AI coding
agents. It is intentionally narrow: a decision tree mapping user intent to
command, a JSON-fields cheat sheet, the condition syntax, and a handful of
workflow patterns (first-contact, point-in-time, transaction extraction,
unexpected-state hunt, clock/reset sanity). Everything else — install,
flags, time syntax, value formatting, known differences — lives in this
README and the skill points back to it.

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
  lib.rs          module declarations + crate VERSION (from CARGO_PKG_VERSION)
  cli.rs          argument grammar, help text, validation
  commands.rs     the seven commands, text + JSON emitters
  model.rs        Wave: signal table, heap-merge replay, bounded streaming
  backend/
    mod.rs        WaveformBackend trait + neutral types
    wellen_backend.rs   wellen adapter (VCD/FST/GHW)
    plugin_backend.rs   drives a C-ABI vtable (built-in or external) as a backend
  plugin/
    ffi.rs        Rust mirror of the C ABI (include/rwave_backend.h)
    loader.rs     $RWAVE_PLUGIN_<EXT> discovery + user-facing error strings
    builtin/      compiled-in vtables — wlf/ (libwlf), fsdb/ (Verdi NPI)
  format.rs       fmt_val, time parse/format, Python-repr quoting
  filter.rs       glob-lite + substring signal matching
  condition.rs    search condition parsing/evaluation, big-uint compare
  json.rs         compact JSON builder (matches Python json.dumps separators)
  main.rs         argv → Wave::open → dispatch, exit codes, SIGPIPE
vendor/
  wellen/         vendored parser front-end
  fst-reader/     vendored + locally patched FST reader
verify/
  stimulus_src/   Verilog testbenches (committed source)
  stimulus/       generated VCD+FST pairs (metadata-normalized)
  fixtures/       small handcrafted traces for differential tests
  run.sh          self-test harness (smoke + VCD/FST parity)
  differential.sh parity check vs the reference Python tool
scripts/
  build-release.sh   cross-build the four release binaries
  gen-stimulus.sh    regenerate stimulus/ from stimulus_src/, sanitized
skill/
  SKILL.md           agent-skill descriptor (decision tree + workflow patterns)
.github/workflows/
  ci.yml          test + verify on push / PR
  release.yml     build + publish the four binaries on v* tags
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
cargo test                  # unit tests (formatting, filters, conditions, CLI)
bash verify/run.sh          # smoke + VCD/FST parity on the bundled stimulus
bash verify/differential.sh # behavioural parity vs the reference Python tool
```

`verify/run.sh` requires only the built binary. It checks that every command
runs on both a VCD and an FST, and that the value-bearing commands produce
identical results across formats for the same design.

`verify/differential.sh` compares `rwave` against the reference `vcd_analyzer.py`
across all seven commands on the fixtures and edge-case designs (136 cases). It
locates the reference via `$VCD_ANALYZER`, `../VCD_ANALYZER/vcd_analyzer.py`, or
`$PATH`, and **skips cleanly (exit 0)** if none is found, so it is safe to run
in a clone or CI without the reference. The documented differences below (the
`list --verbose` id field, the "waveform file" wording, and `dump`
intra-timestamp ordering) are recognized and tolerated; any other divergence is
a failure. Run with `VERBOSE=1` to print a diff for each failure.

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
* **Weak-strength logic levels `h`/`l` in `search --condition`.** Per the VCD
  spec, `h`/`l` are 1/0 with weak drive strength. `rwave` treats them as
  defined logic levels throughout — `normalize_4state` maps `h→1`/`l→0`, so a
  signal carrying value `1h` matches `--condition sig=3` (numerically `11`).
  The Python reference's `val_to_int` rejects any character other than `0`/`1`
  for numeric conversion, so the reference reports no match in that scenario.
  This is the same `normalize_4state` policy rwave applies in `fmt_val`; we
  keep it consistent across the analyzer rather than emulate the reference's
  stricter parser quirk.
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
