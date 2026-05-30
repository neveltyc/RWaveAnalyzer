---
name: waveform-debug
description: VCD/FST waveform analysis for RTL debug. Use when the user has a .vcd or .fst file and wants to inspect, search, compare, or summarize digital simulation waveforms. Triggers include any mention of VCD or FST files, waveform analysis, signal dump inspection, RTL debug, simulation results, value change dump, or specific signal queries like "what is the value of X at time Y", "when does valid go high", "compare state at T1 vs T2", "find all AXI handshakes". Also triggers when the user uploads a .vcd or .fst file or references one by path. Do NOT use for FSDB, SHM, WLF, or other vendor-proprietary formats — those need converting to VCD or FST first.
---

# rwave — agent skill

`rwave` is a single static binary that parses VCD and FST and exposes seven
query commands. **Always pass `--json` from an agent.** Prefer FST —
typically 10x smaller than VCD. Output keys, time units, filter syntax, and
value formatting are documented in the repo README; this file covers only
what is unique to driving the tool from an agent.

## Install

Static binaries are attached to every tagged release. Pick the arch matching
the runtime and `chmod +x`:

```bash
curl -fsSL -o ~/.local/bin/rwave \
  https://github.com/neveltyc/RWaveAnalyzer/releases/latest/download/rwave-linux-amd64
chmod +x ~/.local/bin/rwave
~/.local/bin/rwave --version
```

Other assets on the same release page: `rwave-linux-arm64`,
`rwave-windows-amd64.exe`. Each has a matching `.sha256`.

## Pick the right command

```
User wants to know...
├─ "What's in this file?"
│   └─ info           file overview, signal count, time span, scopes
├─ "What signals exist?" / "Find signals matching X"
│   └─ list           signal paths with width and type
├─ "What happened between T1 and T2?"
│   └─ dump           value-change events in time order
├─ "Which signals are active/static?"
│   └─ summary        per-signal change count, edges, unique values
├─ "What is the value of X at time T?"
│   └─ snapshot       all known signal values at one time point
├─ "What changed between T1 and T2?"
│   └─ compare        diff of signal values at two time points
└─ "When does condition C hold?" / "Find handshakes"
    └─ search         condition-based, three sub-modes:
        ├─ interval   time ranges where condition is true (no --show, no --changed)
        ├─ segment    intervals + observed values         (with --show)
        └─ event      fires when one signal transitions   (--changed SIG)
```

`search`'s JSON top-level key depends on the mode: `intervals` /
`segments` / `events`. Always check `mode` before parsing.
`--changed` takes one signal pattern, not comma-separated.

## Condition syntax (search only)

Comma-separated AND list. Each item is `SIG=VAL`, `SIG==VAL`, or `SIG!=VAL`.

- Signal pattern must resolve to **exactly one** signal. If ambiguous,
  the error lists candidates — use a more specific path.
- Values: decimal (`5`), hex (`0xff`), binary (`b1010` / `0b1010`),
  4-state (`b1x0z`), or bare `x`/`z`.
- `!=` does **not** match `x`/`z` ("unknown is not evidence of
  difference"). To find unknowns, ask explicitly with `sig=x`.
- No OR. Run two searches and merge.

## Command quick reference

`<F>` is the input file. See the repo README for the full surface; the table
below is the agent-side cheat sheet of the JSON-form arguments and the
fields you'll usually parse out.

| Command | Common invocation | Useful JSON fields |
|---|---|---|
| `info` | `rwave --json info <F>` | `signal_count`, `time_min_ticks`, `time_max_ticks`, `duration_h`, `timescale`, `scopes[]`, `var_types` |
| `list` | `rwave --json list <F> [--filter K]` | `signals[].path`, `signals[].width`, `signals[].type` |
| `dump` | `rwave --json dump <F> --begin T --end T --filter K` | `events[].time_ticks`, `events[].time_h`, `events[].path`, `events[].value` |
| `summary` | `rwave --json summary <F> [--filter K]` | `rows[].path`, `rows[].kind`, `rows[].changes`, `rows[].rise_count`/`fall_count`, `rows[].init`, `rows[].last`, `active`, `static` |
| `snapshot` | `rwave --json snapshot <F> --at T [--filter K]` | `signals[].path`, `signals[].value`, `at_ticks`, `at_h`, `known`, `undefined` |
| `compare` | `rwave --json compare <F> --at T1,T2 [--filter K]` | `diffs[].path`, `diffs[].at_t1`, `diffs[].at_t2`, `time1_ticks`, `time1_h`, `time2_ticks`, `time2_h` |
| `search` | see decision tree above | `mode`, then one of `intervals[]` / `segments[]` / `events[]` |

For `dump`, **always pass `--begin/--end` and `--filter`** — running it
unbounded on a large dump streams the whole file.

Filter patterns: substring (`clk`), suffix glob (`*_valid`), prefix glob (`top.u_dma.*`).
`list` shows all aliases of matched signals, not only the matching paths.
A signal hit once may surface dozens of alias rows — use `--verbose` to group by `id`.


## Workflow patterns

(all assume `--json`)

### First contact with a waveform file

```
1. info                        learn time range, scopes, timescale
2. list --filter <suspect>     find the signals of interest
3. summary --filter <window>   spot active vs static signals
4. dump or search              drill into specifics
```

### "What happened at time T?"

```
1. snapshot --at T
2. dump --begin T-Δ --end T+Δ
3. compare --at T-Δ,T+Δ
```

### Protocol transaction extraction (AXI, AHB, etc.)

```
1. list --filter '*valid,*ready,*addr,*data,*len'
2. search --condition "arvalid=1,arready=1" --show araddr,arlen
3. search --condition "wvalid=1,wready=1" --show wdata,wstrb
```

`search` segment mode is the primary tool here — one row per
sub-interval, with `--show` capturing the field values you care about.

### Hunt an unexpected state

```
1. search --condition "state=x"          when does it go unknown?
2. search --condition "error!=0"         when does it assert?
3. snapshot --at <first_hit>             full picture at that moment
4. dump --begin <pre> --end <hit> --filter <relevant>
```

### Clock/reset sanity

```
summary --filter clk,rst,reset
# clk should toggle with balanced rise/fall
# rst should be static after the initial assertion
```

## Agent-side gotchas

- **Output truncation.** Default `--limit` is 200. If `truncated: true`,
  there are more rows — either re-run with `--limit 0` (unlimited) or a
  larger value. `total_is_exact: false` means `total` is a lower bound,
  not the true count.
- **`search` mode discriminator.** The output's top-level array key
  depends on the mode (`intervals` / `segments` / `events`). Always read
  the `mode` field first.
- **Exit code is non-zero on errors.** Errors are a single line on stderr
  starting with `Error:`. Catch and parse them.
- **`--json` everywhere.** Mixing text-mode parsing in is the most common
  source of fragility. Pass `--json` on every invocation.

## Documented behaviors that may surprise

- `dump`'s ordering of *simultaneous* events follows declaration order
  (not VCD writer-emission order). Set of events, timestamps, values are
  identical to the reference; only intra-timestamp order can differ.
- `comments` is always `[]` and `synthesized_buses` is always `0` 
- A zero-width `search` window (`--begin T --end T`) yields no rows.

For everything else (time syntax, filter syntax, value formatting,
format quirks, the FST `parameter`-value drop, performance notes) see
the repo README.

