# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

_No unreleased changes._

## [0.0.1] — 2026-05-30

First public release. See the [README](README.md) for the command surface,
install, and known differences from the Python reference; this entry only
records what is unique to 0.0.1.

### Highlights

- Seven `--json`-aware commands: `info`, `list`, `dump`, `summary`,
  `snapshot`, `compare`, `search` (interval / segment / event modes).
- VCD and FST input via vendored `wellen`; pure-Rust, zero runtime deps.
- Binary-heap k-way merge replay (`O(n log k)`), per-signal binary search
  for `snapshot`/`compare`, memory-bounded streaming for whole-file commands.

### Artifacts

- `rwave-linux-amd64` — static musl
- `rwave-linux-arm64` — static musl
- `rwave-windows-amd64.exe` — MinGW, no extra DLLs
- One `.sha256` per binary

### Fixed (pre-tag bug scan vs. the Python reference)

- `parse_time`: silent saturation at the `i64::MAX as f64 == 2^63`
  boundary; tightened `>` to `>=`.
- `search` interval/segment: emitted nothing when conditions held throughout
  `[--begin, --end]` and no events fell past `--begin`. Now emits the full
  `[t0, t1)` interval; zero-width windows stay silent (`t0 < t1` guard).
- `cli`: `--version` / `--help` pre-scan no longer hijacks the value of a
  preceding flag (e.g. `--filter --version`).
- `pyrepr`: escape `\\`, `\n`, `\r`, `\t`, ASCII `C0` + `DEL` +
  Latin-1 `C1` + `NBSP` to match CPython `unicode_repr`.
- `condition`: weak-strength `h`/`l` handled consistently across `==`/`!=`
  (rwave maps `h→1`, `l→0` per VCD spec; differs from Python's `val_to_int`
  which rejects them — see README "Known differences").
- `condition`: `Op::Ne` on non-logic (real/string/event) signals no longer
  always returns false.

### Vendored

- `wellen` (BSD-3-Clause) — parser front-end.
- `fst-reader` (BSD-3-Clause) — plus a local fix for an out-of-bounds crash
  on FSTs with sparse/aliased signal handles (observed with VCS output).
