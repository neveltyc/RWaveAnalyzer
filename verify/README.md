# verify/ — correctness regression net

Two harnesses, two stimulus sets. Together they form the project's
"does it still do the right thing" baseline. Performance lives in
[`bench/`](../bench).

## Harnesses

| script | what it checks | reference required |
|---|---|---|
| `run.sh` | every command runs on both VCD and FST without error, and the value-bearing commands produce identical output across the two formats for the same design | no — only needs the built `rwave` |
| `differential.sh` | byte-level output parity against the Python reference (`vcd_analyzer.py`); the few documented divergences (`list --verbose` id field, dump intra-timestamp ordering, "waveform file" wording) are tolerated by signature, anything else is a failure | yes — locates `vcd_analyzer.py` via `$VCD_ANALYZER`, `../VCD_ANALYZER/`, or `$PATH`; skips cleanly if absent |

Run:

```sh
cargo build --release
bash verify/run.sh                                # always works
bash verify/differential.sh                       # skips cleanly without the Python ref
VCD_ANALYZER=/path/to/vcd_analyzer.py bash verify/differential.sh   # explicit
```

Expected: `RESULT: PASS=N FAIL=0`. The exact `N` is stable per stimulus
set (currently 106 for `run.sh`, 136 for `differential.sh`).

## Stimulus sets

| dir | how it was made | who consumes it |
|---|---|---|
| `fixtures/` | hand-crafted small VCDs (≤1 KB) targeting one edge case each: bus ranges, escaped identifiers, search predicate fodder, handshake protocol, a minimal baseline | `differential.sh` (the core 65-case loop) |
| `stimulus/` | iverilog-generated VCD+FST pairs from `stimulus_src/`; `$date`/`$version` blocks are normalized at generation so committed bytes are host-independent | `run.sh` (smoke + VCD/FST parity), `differential.sh` (the search/handshake-protocol cases) |
| `stimulus_src/` | the SystemVerilog testbenches behind `stimulus/`. Five designs, each isolating one source of subtle behavior: `counter_fsm` (params and statics), `handshake_proto` (search-friendly valid/ready), `hier_deep` (nested scopes), `real_event` (non-logic value kinds), `xz_tristate` (4-state x/z handling) | `scripts/gen-stimulus.sh` reproduces `stimulus/` from these |

## Regenerating `stimulus/`

Only when a `.v` testbench is intentionally changed:

```sh
scripts/gen-stimulus.sh
```

Requires `iverilog`, `vvp`, `vcd2fst`. The script normalizes `$date` and
`$version` so the committed VCDs/FSTs are byte-identical across machines.

## Adding a new design

1. Drop `verify/stimulus_src/<name>.v` (Verilog or SystemVerilog).
2. Append `<name>` to the `tbs=(…)` list in `scripts/gen-stimulus.sh`.
3. Append `<name>` to the `designs=(…)` list in `verify/run.sh`.
4. Run `scripts/gen-stimulus.sh` to populate `verify/stimulus/<name>.{vcd,fst}`.
5. Run `verify/run.sh` to confirm parity. Commit the new `.v` plus the
   generated waveform pair.
