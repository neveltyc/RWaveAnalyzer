# bench/ — cross-version performance baseline

Three files do the work:

| file | role |
|---|---|
| `stress.fst.xz` | committed dataset (~160 KB compressed, ~2 MB decompressed). 4,394 signals, 77M value changes, 1.5 ms simulated. Deterministic. |
| `gen.py` | how `stress.fst.xz` was produced. Pure Python via [`pylibfst`](https://pypi.org/project/pylibfst/). Not run in CI. |
| `run.py` | the benchmark itself. Auto-decompresses the dataset and runs `rwave` under GNU time for nine commands. Used by the CI bench workflow. |

## Run the bench

```sh
cargo build --release           # builds target/release/rwave
python3 bench/run.py             # auto-decompresses, runs the nine commands
```

No Python deps required for `run.py` itself — it shells out to `rwave` and
parses `xz` / GNU time output. On macOS install GNU time once with
`brew install gnu-time`; on Linux distros it's the `time` package.

## Regenerate the dataset (rare)

Only when the activity model is intentionally changed:

```sh
python3 -m venv /tmp/v && /tmp/v/bin/pip install pylibfst
/tmp/v/bin/python bench/gen.py --size medium
xz -9 -k bench/build/stress.fst
mv bench/build/stress.fst.xz bench/stress.fst.xz
```

`gen.py` has three presets (`small` ~5 MB, `medium` ~30 MB pre-xz,
`huge` ~100 MB). The committed dataset is `medium`. **Always re-run the
bench after regenerating** so the comparison baseline tracks reality.

## What the dataset models

A simplified 4-cluster × 16-core SoC with three activity tiers:

- **Tier 1** — clocks, pipeline stages, retire — change every cycle.
- **Tier 2** — buses, ALU/LSU operands, NoC — change ~10% of cycles.
- **Tier 3** — regfile entries, CSRs, exception flags — change <0.1% of cycles.

This mirrors what real chip dumps look like at scale: many thousand signals
declared, most mostly idle, a few clocks driving the bulk of value changes.
