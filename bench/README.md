# bench/ — cross-version performance baseline

Two files do the work:

| file | role |
|---|---|
| `stress.fst.xz` | committed dataset (~8 MB compressed, ~63 MB decompressed). A real Verilator-generated FST from a [VeeRwolf](https://github.com/chipsalliance/Cores-VeeR-EL2) RISC-V SoC running [Zephyr RTOS](https://github.com/zephyrproject-rtos): 10,220 unique signals, 20 µs of simulated boot, deep AXI/Wishbone/JTAG/UART hierarchy. |
| `run.py` | the benchmark itself. Auto-decompresses the dataset and runs `rwave` under GNU time across nine practical agent queries; emits a compact 3-row Markdown table + JSON dump. |

## Run the bench

```sh
cargo build --release           # builds target/release/rwave
python3 bench/run.py            # auto-decompresses, runs the nine commands
```

No Python deps required for `run.py` — it shells out to `rwave` and parses
`xz` / GNU time output. On macOS install GNU time once with
`brew install gnu-time`; on Linux distros it's the `time` package.

The decompressed FST (`bench/stress.fst`, ~63 MB) is git-ignored and
recreated on first run.

## What's measured

Every command is **scoped via `--filter` or `--condition`** to one module —
this mirrors realistic agent use. Unfiltered whole-file commands on this
trace consume **7–8 GB peak RSS** and run for 50+ seconds; they're
deliberately *not* in the bench because they don't fit standard 7 GB CI
runners. If you want to verify those numbers locally:

```sh
# locally only — needs > 8 GB RAM and ~60s
gtime -f "%e s, %M KB" target/release/rwave --json summary bench/stress.fst
```

## Trace provenance

The FST was captured from a real open-source simulation run by
[@neveltyc](https://github.com/neveltyc):
[VeeRwolf](https://github.com/chipsalliance/Cores-VeeR-EL2) (Verilator)
running a Zephyr RTOS boot image, 20 µs of simulated time. The original
~63 MB FST was re-compressed with `xz -9` (single-stream, no tar wrapper)
to fit the repo at ~8 MB.

Because the trace is deterministic and fully checked in, cross-version
rwave benchmark comparisons are reproducible: same bytes, same numbers
modulo host CPU noise.
