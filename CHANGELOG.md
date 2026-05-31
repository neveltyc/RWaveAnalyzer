# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

_No unreleased changes._

## [0.0.2] — 2026-05-31

### Highlights

- **New build target: macOS Apple Silicon** (`rwave-macos-arm64`). Built
  natively on `macos-latest` CI runners; Intel macOS deliberately not
  shipped.
- **Cross-version performance baseline** (`bench/`) on a real Verilator
  capture of [VeeRwolf](https://github.com/chipsalliance/Cores-VeeR-EL2)
  RISC-V EL2 core + [Zephyr RTOS](https://github.com/zephyrproject-rtos)
  boot — 10 k signals, 20 µs of simulation, ~63 MB FST. A new GitHub
  Actions workflow (`bench.yml`) runs the harness on every `v*` tag and
  appends the results to the release body.

### Changed

- Truncation messages on `list` / `dump` / `summary` / `search` now end
  with `(use --limit 0 to see all)`.
- `list` reports `no match; try a broader filter or run without --filter
  to browse` when the filter selects zero signals.
- `list --help` row now reads `[--filter K1,K2]  List signals (filter
  matches any alias path)` to reflect what the filter actually does.
- README + agent skill clarify that `--filter` matches *any alias path*
  of a signal — one logical signal can surface many alias rows, and
  `--verbose` lets the consumer collapse them by `id`.

### Artifacts

- `rwave-linux-amd64` — static musl
- `rwave-linux-arm64` — static musl
- `rwave-macos-arm64` — Apple Silicon native
- `rwave-windows-amd64.exe` — MinGW, no extra DLLs
- One `.sha256` per binary

### Internal

- Stimulus cleanup: `verify/stimulus/{edge_cases,wide_bus}` removed —
  their coverage overlapped; the remaining 5 designs each isolate one
  unique source of subtle behavior. Case counts: `verify/run.sh`
  150 → 106, `verify/differential.sh` 150 → 136 (both still
  `PASS=N FAIL=0`).
- `verify/differential.sh` now tolerates the new truncation hint via a
  normalize rule rather than scoring it as a real divergence.
- Agent skill (`skill/SKILL.md`): substantial agent-driven refinements
  from field use — decision-tree, JSON field names, condition semantics,
  multi-signal dump for timeline correlation, event-driven workflow
  pattern.
- `bench/`, `verify/`: new `README.md` in each describing the directory's
  role; the synthetic generator (`bench/gen.py`) is gone — the committed
  dataset is a real run, not procedurally-built.
- `scripts/build-release.sh`: cleanly refuses when asked to build a
  `macos-*` target from a non-Darwin host (cross-compile to Darwin needs
  the Apple SDK and is intentionally out of scope) instead of failing
  partway with a cryptic linker error.

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
