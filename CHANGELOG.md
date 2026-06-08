# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

_No unreleased changes._

## [0.0.4] — 2026-06-08

### Fixed

- **Windows plugin discovery.** `pip install`ed plugins were never
  auto-discovered on Windows — only the `$RWAVE_PLUGIN_<F>` env var
  worked. rwave probed the wrong filename and the wrong paths:
  - The probed cdylib name carried a bogus `lib` prefix. Windows
    cdylibs are `rwave_<f>_backend.dll`, not `librwave_<f>_backend.dll`.
  - A venv keeps packages in `…\Lib\site-packages` (no `pythonX.Y`
    level); the scan assumed the Unix `lib/pythonX.Y/site-packages`
    shape and missed them.
  - `pip install --user` lands in
    `%APPDATA%\Python\Python3XX\site-packages`, which was never
    scanned. It now is.
  Linux discovery is unchanged.

### Changed

- `linux-amd64` release binaries are pinned to the glibc 2.17
  (manylinux2014) baseline again. A native-build shortcut on the CI
  runner had bypassed the zigbuild `.2.17` pin and shipped a binary
  requiring glibc 2.34. `release.yml` now also asserts the baseline
  with `objdump` and fails if any symbol needs glibc > 2.17.

### Docs

- `docs/PLUGIN.md` and `README.md`: corrected the Windows plugin
  filename (no `lib` prefix) and documented the per-platform
  site-packages layouts.

## [0.0.3] — 2026-06-02

### Highlights

- **External-format plugins.** rwave can now load waveform formats
  beyond the built-in VCD/FST/GHW by `dlopen`ing a plugin shared
  library at runtime — see [`docs/PLUGIN.md`](docs/PLUGIN.md) and the
  C header [`crates/rwave/include/rwave_backend.h`](crates/rwave/include/rwave_backend.h)
  for the protocol. rwave itself ships no plugin implementation; the
  C ABI is the public contract.

### Added

- `crates/rwave/include/rwave_backend.h` — public C ABI. One exported
  symbol per plugin: `rwave_backend()` returning a const vtable.
  Versioning lives in the vtable's `abi_version` field, not the symbol
  name. `RWAVE_BACKEND_ABI_VERSION = 1`.
- `crates/rwave/src/plugin/` — discovery (`$RWAVE_PLUGIN_<FORMAT>`
  env var, then site-packages scan) keyed on the file extension, and
  the four user-facing error variants (`PlatformUnsupported`,
  `NotInstalled`, `AbiMismatch`, `LoadFailed`). No format registry —
  the convention "extension `<ext>` is served by the plugin packaged
  as `rwave_<ext>`" is the whole protocol; adding a new format is a
  plugin-side concern with no rwave change required.
- `crates/rwave/src/backend/plugin_backend.rs` — generic
  `WaveformBackend` forwarder that talks to the vtable and adapts
  streamed (`sid`, `time`, `value`) emit calls into per-signal
  `SignalTrace`s.
- `Wave::open` now dispatches by extension: `.vcd` / `.fst` / `.ghw`
  (or no extension) → built-in `wellen` backend; any other extension
  → plugin loader path.
- README gains a "Plugin formats" section.

### Versioning model (three independent counters)

| Counter | Owner | Bumps when |
|---|---|---|
| rwave version | rwave | any rwave change |
| plugin version | plugin author | any plugin change (vendor lib update, decoder fix) |
| ABI version | this protocol | breaking vtable changes only |

The wheel filename in the "not installed" hint is intentionally
version-agnostic — coupling rwave's version into it would falsely
imply that bumping rwave forces a plugin rebuild, which it does not
unless the ABI itself bumps.

### Platform support for the plugin path

Compile-time gated to `linux x86_64` and `windows x86_64`. On other
targets (linux-arm64, macos) opening a non-built-in extension produces
a clean "extension is not supported on this platform" error without
attempting any filesystem or process work. Built-in VCD/FST/GHW paths
are unaffected.

### Changed

- `Cargo.toml` — version bump to `0.0.3`.
- New dependency: `libloading 0.8` (used only on platforms where the
  plugin path is enabled; the stub on other targets does not call it).
- `rwave-linux-amd64` switched from musl-static to glibc-dynamic
  (manylinux2014 baseline) so it can `dlopen` plugins; static musl
  libc does not implement `dlopen`. `rwave-linux-arm64` stays static
  (plugins are compile-time gated off on aarch64). Alpine / musl-only
  x86-64: build from source.
- Release binaries are now built with `--remap-path-prefix` for
  `$HOME`, cargo, and rustup paths, so third-party crate source paths
  no longer leak the build host's user and directory layout.

### Internal

- `cargo test`: +3 unit tests covering `LoadError` text shapes
  (`error_platform_unsupported_message`,
  `error_not_installed_message_is_version_agnostic`,
  `error_abi_mismatch_message_mentions_both_versions`).
- Defensive bound on `Vec::set_len` after the plugin's `var_decls`
  call: clamp `written` to the previously-queried `total` so a
  misbehaving plugin cannot drive `set_len` past capacity.

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
