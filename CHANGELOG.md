# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

_No unreleased changes._

## [0.0.1] — 2026-05-30

First public release. A Rust waveform analyzer for VCD and FST whose
command-line surface mirrors the reference `VCD_ANALYZER` Python tool.

### Commands

- Seven commands — `info`, `list`, `dump`, `summary`, `snapshot`, `compare`,
  `search` — each with a human-readable text form and a compact `--json` form
  whose keys and separators match the reference tool.
- VCD, FST, and GHW input via a vendored `wellen` front-end, with format
  auto-detection.
- Time handling with `fs/ps/ns/us/ms/s` suffixes and raw-tick integers;
  unit-suffixed times are scaled by the file timescale using banker's rounding
  to match Python's `round()`. Bare integer ticks accept Python-style `_` digit
  separators (`1_000`, `1_0_0_0`). Out-of-range bare-integer ticks report
  `time value too large; got '…', max ticks is 9223372036854775807`, matching
  the reference rather than the generic "invalid" error.
- Value formatting covering scalars, multi-bit vectors (decimal + zero-padded
  hex), 4-state `x`/`z` vectors, reals, strings, and events, plus
  arbitrary-width buses via an internal big-unsigned-integer type.
- Signal selection via a glob-lite matcher (`*`, `?`) and case-insensitive
  substring matching; conditional `search` with `=`, `==`, `!=` operators and
  multi-condition predicates.

### Architecture

- Layered design: `cli` → `commands` → `model` → `backend`, with `format`,
  `filter`, `condition`, and `json` as backend-agnostic leaf utilities. The
  parser is isolated behind a `WaveformBackend` trait so additional formats can
  be added without touching the command set; `wellen` is the only
  implementation today.

### Performance

- Value-change replay uses a binary min-heap k-way merge (`O(n log k)`),
  replacing a linear-scan merge.
- `snapshot`/`compare` use per-signal binary search rather than full replay.
- Whole-file commands (`summary`, and unfiltered `dump`/`snapshot`/`compare`)
  stream their work in memory-bounded batches, decoding and releasing signal
  traces per batch so very large files (hundreds of thousands of signals) stay
  within a few-GB working set. `dump` retains only the earliest `--limit`
  events via a bounded heap. These paths are output-identical to the eager
  paths.
- Logic values are materialized into a small inline string (`BitStr`) that
  keeps short values (~99% of changes) off the heap, cutting decode-time heap
  traffic by ~13% on a 222k-signal FST with byte-identical output.
  Signal-table construction is ~28% faster via a single `full_name` per
  variable, `FxHashMap` grouping, and an unstable final sort.

### Release tooling

- `scripts/build-release.sh` cross-builds release binaries for three
  deployment targets via `cargo-zigbuild`:
  - `linux-amd64`   → `x86_64-unknown-linux-musl`   (fully static)
  - `linux-arm64`   → `aarch64-unknown-linux-musl`  (fully static)
  - `windows-amd64` → `x86_64-pc-windows-gnu`       (no extra DLLs)
- A GitHub Actions release workflow builds and uploads all three artifacts
  on every `v*` tag.

### Fixed

- **Vendored `fst-reader` out-of-bounds crash.** Upstream `fst-reader` 0.16.6
  sizes the signal include-bitmask in `read_signals` from the number of
  distinct signals (and the declared max var-id code), but some writers
  (observed with VCS-generated FSTs) emit value-change geometry whose handle
  indices exceed both. Reading any signal value from such a file panicked with
  an index-out-of-bounds in the bitmask. The vendored copy sizes the mask to
  also cover the largest handle present in the filter, eliminating the crash.
  This is the only functional change to the vendored parser.

### Testing

- Five Verilog testbenches plus two FST-focused designs, compiled to matched
  VCD+FST pairs. `$date`/`$version` blocks are normalized at generation time so
  committed stimuli carry no host or wall-clock metadata.
- `verify/run.sh` self-test harness covering command smoke and VCD/FST output
  parity on the bundled stimulus.
- `verify/differential.sh` parity check against the reference Python tool;
  skips cleanly when the reference is absent.

### Known differences from the reference tool

- VCD-specific wording generalized to format-neutral phrasing (e.g. "cannot
  open waveform file").
- `list --verbose` reports the backend signal index rather than the raw VCD
  identifier code (the abstract backend does not expose identifier codes).
- `comments` is empty and `synthesized_buses` is 0 (no `wellen` equivalent).
- `vcd2fst` does not carry Verilog parameter/localparam values into the FST;
  `rwave` reports whatever each file actually contains.
- `dump` event order within a single timestamp follows declaration order
  rather than the writer's VCD emission order. Values, timestamps, and the set
  of emitted events are identical; only the order of simultaneous events can
  differ, and only for `dump`. `wellen` stores changes per-signal and does not
  retain cross-signal file order, so matching the reference exactly is not
  possible without diverging from upstream `wellen`. (The reference is
  VCD-only and cannot read FST.)
