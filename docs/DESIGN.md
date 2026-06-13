# Design notes

Internals, performance characteristics, and compatibility behavior — for
contributors and the curious. For building see [BUILD.md](BUILD.md); for the
backend ABI see [PLUGIN.md](PLUGIN.md).

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

The same boundary is how vendor and external formats plug in: a backend can come
from a compiled-in C-ABI vtable (`plugin/builtin/`) or a `dlopen`ed cdylib
(`plugin/loader.rs`), both driven through `plugin_backend.rs`. See
[PLUGIN.md](PLUGIN.md) for the ABI.

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
  wellen/         vendored parser front-end (BSD-3-Clause)
  fst-reader/     vendored + locally patched FST reader (BSD-3-Clause)
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
  bench.yml       performance baseline, appended to the release after publish
```

## Performance

- **Replay**: a binary min-heap k-way merge over selected signals' traces,
  `O(n log k)` for `n` changes across `k` signals; ties within a timestamp
  resolve to writer (declaration) order.
- **Snapshots / compare**: per-signal binary search for the last value at or
  before the target time — no full replay.
- **Whole-file commands** (`summary`, and unfiltered `dump`/`snapshot`/`compare`)
  decode signals in memory-bounded batches and release each batch, so peak
  memory is proportional to a batch rather than the whole file. `summary`
  computes its per-signal-independent statistics directly from each trace with
  an allocation-light inner loop. `dump` retains only the earliest `--limit`
  events via a bounded heap.

These paths produce byte-identical output to the simple eager paths; the switch
is purely a memory/throughput optimization keyed on selection size.

The vendored `fst-reader` additionally carries a local fix for an upstream
out-of-bounds crash on FSTs with sparse/aliased signal handles (such as VCS
output); see `CHANGELOG.md`.

## Testing

```sh
cargo test                  # unit tests (formatting, filters, conditions, CLI)
bash verify/run.sh          # smoke + VCD/FST parity on the bundled stimulus
bash verify/differential.sh # behavioural parity vs the reference Python tool
```

`verify/run.sh` requires only the built binary: it checks that every command
runs on both a VCD and an FST, and that the value-bearing commands produce
identical results across formats for the same design.

`verify/differential.sh` compares `rwave` against the reference `vcd_analyzer.py`
across all seven commands on the fixtures and edge-case designs (136 cases). It
locates the reference via `$VCD_ANALYZER`, `../VCD_ANALYZER/vcd_analyzer.py`, or
`$PATH`, and **skips cleanly (exit 0)** if none is found, so it is safe to run in
a clone or in CI without the reference. Run with `VERBOSE=1` to print a diff for
each failure.

## Compatibility with VCD_ANALYZER

`rwave`'s CLI mirrors the reference Python tool
[`VCD_ANALYZER`](https://github.com/neveltyc/VCD_ANALYZER), so the two are
drop-in compatible at the command line. A few differences are intentional, and
stem from using a real parser front-end and from generalizing beyond VCD:

- **Format-neutral wording.** Messages that named "VCD" specifically are
  generalized (e.g. `cannot open waveform file`), since `rwave` handles multiple
  formats.
- **`list --verbose` identifier field.** The reference prints the raw VCD
  identifier code (`!`, `$`, …). `rwave` reports the backend's signal index
  instead, because the abstract backend does not expose VCD identifier codes.
- **Comments / synthesized buses.** `wellen`'s reader does not preserve VCD
  `$comment` blocks, so `comments` is empty; `synthesized_buses` is reported as
  0 (no backend equivalent).
- **FST conversion artifact (tooling, not `rwave`).** `vcd2fst` does not carry
  Verilog `parameter`/`localparam` *values* into the FST. A design that declares
  such constants will show them with values in its VCD but without values in the
  converted FST; `rwave` faithfully reports whatever each file actually contains.
- **Weak-strength logic levels `h`/`l` in `search --condition`.** Per the VCD
  spec, `h`/`l` are 1/0 with weak drive strength. `rwave` treats them as defined
  logic levels throughout — `normalize_4state` maps `h→1`/`l→0`, so a signal
  carrying value `1h` matches `--condition sig=3` (numerically `11`). The Python
  reference's `val_to_int` rejects any character other than `0`/`1` for numeric
  conversion, so the reference reports no match in that scenario. This is the
  same `normalize_4state` policy rwave applies in `fmt_val`; we keep it
  consistent across the analyzer rather than emulate the reference's stricter
  parser quirk.
- **`dump` event order within a single timestamp.** When several signals change
  at the *same* time, the reference emits them in the order their value-changes
  physically appear in the VCD (which depends on the writer — Icarus Verilog,
  for instance, emits its initial `$dumpvars` block in reverse-declaration
  order). `wellen` stores changes per-signal and does not preserve that
  cross-signal file order, so `rwave` orders simultaneous events by declaration
  order instead. **All values, timestamps, and the set of emitted events are
  identical** — only the relative order of events sharing a timestamp can differ,
  and only for `dump`. (The reference tool is VCD-only and cannot read FST at
  all, so there is no cross-format reference for FST ordering.)
