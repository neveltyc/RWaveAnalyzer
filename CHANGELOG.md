# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **Out-of-range time values now report "too large" instead of "invalid".** A
  bare integer time exceeding the int64 range (e.g. `99999999999999999999`)
  overflowed Rust's integer parse and fell through to the generic "invalid time
  value" error, whereas the reference reports `time value too large; got '…',
  max ticks is 9223372036854775807`. The bare-integer and unit-scaled paths now
  detect over-range values explicitly (the latter also guards against `as i64`
  saturation), matching the reference. Added regression tests.
- **Accept Python-style `_` digit separators in integer tick values.** The
  reference parses ticks via Python's `int()`, which permits underscores between
  digits (`1_000`, `1_0_0_0`); `rwave` previously rejected them as invalid. Bare
  integer ticks now accept underscores with the same rules Python uses (rejecting
  leading/trailing/doubled underscores, and — like the reference — not accepting
  underscores in unit-suffixed or hex forms). Added regression tests.

### Documented

- **`dump` event order within a single timestamp.** Clarified in the README that
  when multiple signals change at the same time, `rwave` orders them by
  declaration order, while the reference preserves the VCD's value-change
  emission order (writer-specific; Icarus Verilog emits its initial dump in
  reverse-declaration order). Values, timestamps, and the emitted event set are
  identical — only the order of simultaneous events can differ, and only for
  `dump`. `wellen` stores changes per-signal and does not retain cross-signal
  file order, so matching it exactly is not possible without diverging from
  upstream `wellen`. (The reference is VCD-only and cannot read FST.)

### Changed

- **Release build script now targets Linux x86-64 only.** `scripts/build-release.sh`
  builds a fully static musl binary (`dist/rwave-linux-amd64`) by default, with
  a `--flavour glibc` option, and supports cross-building from macOS via
  `cargo-zigbuild` (`--zig`). It checks prerequisites and prints exact install
  commands for anything missing. The Windows (MinGW) cross-target was dropped to
  avoid maintaining a Windows build environment; native `cargo build` still
  works on any platform for development. Added `docs/BUILD.md`.

### Performance

- **Inline bit-strings cut decode-time heap allocation.** Logic values are
  materialized into a small inline string (`BitStr`) that keeps short values
  (~99% of changes) off the heap, instead of allocating a `String` per change.
  Decode of a 222k-signal FST is ~13% faster with byte-identical output. Also
  optimized signal-table construction (~28% faster open) via a single
  `full_name` computation per variable, `FxHashMap` grouping, and an unstable
  final sort.

## [0.1.0] — 2026-05-30

First release. A Rust waveform analyzer for VCD and FST whose command-line
surface mirrors the reference `VCD_ANALYZER` Python tool.

### Added

- Seven commands — `info`, `list`, `dump`, `summary`, `snapshot`, `compare`,
  `search` — each with a human-readable text form and a compact `--json` form
  whose keys and separators match the reference tool.
- VCD, FST, and GHW input via a vendored `wellen` front-end, with format
  auto-detection.
- Time handling with `fs/ps/ns/us/ms/s` suffixes and raw-tick integers;
  unit-suffixed times are scaled by the file timescale using banker's rounding
  to match Python's `round()`.
- Value formatting covering scalars, multi-bit vectors (decimal + zero-padded
  hex), 4-state `x`/`z` vectors, reals, strings, and events, plus arbitrary-width
  buses via an internal big-unsigned-integer type.
- Signal selection via a glob-lite matcher (`*`, `?`) and case-insensitive
  substring matching; conditional `search` with `=`, `==`, `!=` operators and
  multi-condition predicates.
- Static, fully self-contained Linux binary (musl) and a Windows binary
  (MinGW-w64); build automation in `scripts/build-release.sh`.
- Test stimulus: five Verilog testbenches compiled to matched VCD+FST pairs, and
  a `verify/run.sh` self-test harness checking command smoke and VCD/FST output
  parity.

### Architecture

- Layered design: `cli` → `commands` → `model` → `backend`, with `format`,
  `filter`, `condition`, and `json` as backend-agnostic leaf utilities. The
  parser is isolated behind a `WaveformBackend` trait so additional formats can
  be added without touching the command set; `wellen` is one implementation.

### Performance

- Value-change replay uses a binary min-heap k-way merge (`O(n log k)`),
  replacing a linear-scan merge.
- `snapshot`/`compare` use per-signal binary search rather than full replay.
- Whole-file commands (`summary`, and unfiltered `dump`/`snapshot`/`compare`)
  stream their work in memory-bounded batches, decoding and releasing signal
  traces per batch so very large files (hundreds of thousands of signals) stay
  within a few-GB working set. `dump` retains only the earliest `--limit` events
  via a bounded heap. These paths are output-identical to the eager paths.

### Fixed

- **Vendored `fst-reader` out-of-bounds crash.** Upstream `fst-reader` 0.16.6
  sizes the signal include-bitmask in `read_signals` from the number of distinct
  signals (and the declared max var-id code), but some writers (observed with
  VCS-generated FSTs) emit value-change geometry whose handle indices exceed
  both. Reading any signal value from such a file panicked with an index-out-of-
  bounds in the bitmask. The vendored copy sizes the mask to also cover the
  largest handle present in the filter, eliminating the crash. This is the only
  functional change to the vendored parser.

### Known differences from the reference tool

- VCD-specific wording generalized to format-neutral phrasing (e.g. "cannot open
  waveform file").
- `list --verbose` reports the backend signal index rather than the raw VCD
  identifier code (the abstract backend does not expose identifier codes).
- `comments` is empty and `synthesized_buses` is 0 (no `wellen` equivalent).
- `vcd2fst` does not carry Verilog parameter/localparam values into the FST;
  `rwave` reports whatever each file actually contains.
