# Changelog

All notable changes to this project are documented here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.3] — 2026-06-17

### Fixed
- `search --condition` on a real- or string-valued signal no longer matches by
  sniffing the rendered value characters. A real valued `100.0` renders as
  `"100"`, which the old heuristic read as the binary vector `100` (= 4) — so
  `dac=4` spuriously matched and `dac=100` missed. Condition values are now
  classified by the signal's declared kind; non-logic signals (real/string/
  event) never satisfy a numeric or bit-pattern target.
- De-flaked the `batch_fatal_exit_codes` integration test: it now tolerates the
  expected `BrokenPipe` from writing stdin to an rwave that has already exited on
  a fatal or usage error. This surfaced as an intermittent CI failure under the
  `--release` test build, where the child exits fast enough to win the race.

### Performance
- Collapsed the three near-identical value enums (`RawValue`, `ValueRef`,
  `OwnedValue`) into a single `RawValue`. Value-change replay
  (`Wave::for_each_event`) now hands back a borrowed `&RawValue` instead of
  allocating a fresh `String` per change, removing 1–2 heap allocations per
  emitted change on the `dump`/`search` hot path (logic values keep `BitStr`'s
  inline, no-alloc storage). `compare` keeps identical value-equality semantics
  by comparing canonical raw strings; all output is byte-for-byte unchanged —
  the verify-suite parity and byte-identical batch tests still pass.

### Internal
- Split the ~2,300-line `commands.rs` into a `commands/` module: one file per
  subcommand (`info`, `list`, `dump`, `snapshot`, `compare`, `summary`,
  `search`), a shared `common` helpers module, and the dispatch in `mod`. Pure
  code movement — command bodies are byte-for-byte unchanged.
- Deduplicated the built-in WLF/FSDB backends: the identical error-string
  helpers were merged into `plugin/builtin/diag.rs` (the `bridge_err` prefix is
  now a parameter), and the identical self-locating `dladdr` /
  `GetModuleHandleEx` logic into `plugin/builtin/self_path.rs`. Removed an
  unused `parent_dir` helper.
- Documented two long-standing design intents to forestall future regressions:
  the deliberate `< t0` (edge) vs `<= t0` (level) `--begin` boundary split
  between `search` event and interval modes, and that the vendored `fst-reader`
  include-mask sizing — not a guard in `is_set`, which indexes unchecked — is
  what keeps sparse VCS-handle lookups in bounds.

## [0.1.2] — 2026-06-15

A batch-mode point release: load a waveform **once** and run many queries
against it, collapsing N cold loads into one — most valuable for large FSDB/WLF
databases, where each open re-initializes a vendor runtime and re-indexes the
whole hierarchy.

### Added
- **`--batch` mode.** `rwave --batch [--json] <file> [global-opts]` loads the
  waveform once, then reads one command per line from stdin and emits one result
  per command, in input order. Each line is a normal command minus the leading
  `rwave` (e.g. `list --filter clk,state`); a trailing `#label` sets that
  result's id, blank and `#`-comment lines are skipped, and `[global-opts]`
  become per-command defaults that a line may override. With `--json` each result
  is one NDJSON object `{"id","ok","result"|"error"}`; without it, each result is
  a `#label` header line followed by that command's normal text output. Intended
  for pre-planned multi-step debugging — CI gates, scripted extraction, AI
  agents — not adaptive interaction.

### Behavior
- A batch `result` is **byte-for-byte identical** to the equivalent
  `rwave --json <cmd> <file> …` call: single-command and batch share the same
  compute and serialization code. A command's success or failure in batch
  matches single-command mode exactly — only genuinely failing commands (signal
  not found, illegal time, missing required argument, bad condition, …) are
  marked `ok:false`, and a failed command no longer aborts the run: the batch
  continues and exits `0`. Failing to load the file or read the command stream is
  fatal (exit `1`); a usage error such as `--batch` together with a subcommand is
  exit `2`. Every single-command invocation behaves exactly as before.

## [0.1.1] — 2026-06-15

A token-economy and independence point release: compact hex values, a
self-contained test suite, and a documented license-free FSDB path.

### Added
- **Documented the source-only [`rwave-open-fsdb-plugin`](https://github.com/neveltyc/rwave-open-fsdb-plugin)**
  FSDB backend (Synopsys FsdbReader via `$RWAVE_PLUGIN_FSDB`): a second `.fsdb`
  path that, unlike the built-in NPI backend, needs no Verdi-Ultra license at
  runtime, and overrides the built-in when set. The README now organizes WLF /
  FSDB / environment-variable docs into clear subsections. (The external-plugin
  mechanism itself shipped in 0.1.0; this release documents the public, cleaned
  plugin.)

### Changed
- **Multi-bit logic values now print as `0x<hex>`** (lower-case, leading zeros
  stripped) instead of `<decimal> (0x<hex>)`. 1-bit (`0`/`1`/`x`/`z`),
  unknown-bit buses (`b<bits>`), and real/string values are unchanged. Hex is
  the compact, agent-facing representation for hardware values and avoids
  re-encoding the (already-known) signal width as padding; decimal is trivially
  derivable and would overflow a JSON number on wide buses. Affects every
  value-bearing command (`dump`, `snapshot`, `compare`, `summary`, `search
  --show`) in both text and `--json` output. `verify/run.sh` (VCD↔FST parity)
  is unaffected.

### Removed
- **The differential harness against the Python reference tool is gone**
  (`verify/differential.sh`), and the source comments framing rwave as a
  field-for-field reimplementation of `vcd_analyzer.py` are dropped. The port is
  validated; rwave is now developed on its own terms, with `cargo test` plus
  `verify/run.sh` (both reference-free) as the regression net. This frees the
  output to diverge from the reference where a better agent-facing shape exists
  — the hex value format above is the first such change.

## [0.1.0] — 2026-06-13

Folds the WLF and FSDB waveform formats into the main binary and
ships multi-platform binaries with the experimental backends gated per target.

### Added
- **Built-in WLF backend** (Mentor/Questa), experimental, linux-amd64: `.wlf`
  read via `libwlf`, located at runtime through `$RWAVE_WLF_LIB`.
- **Built-in FSDB backend** (Synopsys Verdi NPI), experimental, linux-amd64:
  `.fsdb` read via `libNPI` (`$RWAVE_FSDB_LIB`); needs a Verdi install with a
  **Verdi-Ultra** license at runtime. An external backend set via
  `$RWAVE_PLUGIN_FSDB` overrides it.
- Vendor backends bundle no proprietary binaries or EDA-vendor code and link
  nothing at build time — they `dlopen` a user-supplied, user-licensed vendor
  `.so` located via env var. See the README disclaimer.
- `wlf` / `fsdb` Cargo features (default-on; target-gated to amd64 linux); a
  `--no-default-features` build is pure VCD/FST/GHW with no proprietary
  surface.

### Changed
- **External-plugin discovery is now env-var-only.** A non-native
  extension `<ext>` is served by the cdylib named in `$RWAVE_PLUGIN_<EXT>`;
  the wheel / site-packages scan is gone. Built-in and external backends
  share one C-ABI vtable + adapter, and an external override wins over a
  built-in of the same extension.
- **Releases ship four binaries** (`linux-amd64`, `windows-amd64`,
  `linux-arm64`, `macos-arm64`). The experimental WLF/FSDB backends are
  linux-amd64 only, so the windows / arm64 / macOS binaries are pure
  VCD/FST/GHW core.

### Removed
- The pip-wheel / site-packages plugin install path, and its
  `<format> support not installed … wheel …` error.

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
