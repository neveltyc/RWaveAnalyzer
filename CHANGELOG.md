# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

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
