---
name: waveform-debug
description: RTL waveform analysis CLI for debug, CI, and AI agents. Natively reads VCD, FST (preferred — ~10× smaller), and GHW. On linux-amd64, experimental support for WLF and FSDB via each vendor's own reader library. Use when the user has a waveform file (.vcd, .fst, .ghw, .wlf, .fsdb) and wants to inspect, search, compare, or summarize signals — triggers on any mention of waveform analysis, signal queries, RTL debug, simulation results, or VCD/FST/WLF/FSDB files.
---

# rwave — agent skill

`rwave` is a single binary for querying RTL simulation waveforms from the
terminal. It natively reads **VCD**, **FST**, and **GHW** (prefer FST — typically
10× smaller than VCD). On linux-amd64 it also provides experimental support for
**WLF** (Questa/ModelSim) and **FSDB** (Verdi) by calling into each vendor's own
reader library interface. Seven query commands cover inspection, search,
comparison, and summary. **Always pass `--json` from an agent.** This file covers
what is unique to driving the tool from an agent — see the repo README for the
full reference.

## Install

Prebuilt binaries are attached to every tagged release (each with a `.sha256`).
All four read VCD/FST/GHW; only `rwave-linux-amd64` includes experimental
WLF and FSDB support. Pick the one matching the runtime and `chmod +x`:

```bash
curl -fsSL -o ~/.local/bin/rwave \
  https://github.com/neveltyc/RWaveAnalyzer/releases/latest/download/rwave-linux-amd64
chmod +x ~/.local/bin/rwave
~/.local/bin/rwave --version
```

## Vendor formats — experimental (linux-amd64 only)

On linux-amd64, rwave provides experimental support for Questa `.wlf` and
Verdi `.fsdb` by calling into each vendor's own reader library interface.
Point rwave at the library from the user's licensed installation via an
env var, then query as usual:

```bash
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so          # for .wlf
export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"  # for .fsdb (needs a Verdi-Ultra license)
rwave --json info dump.fsdb
```

If the env var is unset, the library/license is missing, or the build is not
linux-amd64, `.wlf`/`.fsdb` fail with a one-line `Error:` — fall back to
converting the dump to VCD or FST first.

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
To catch both edges, run two searches: `!=0` for rising, `=0` for falling.

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
For `snapshot` and `compare` on large files, **always pass `--filter`** — unfiltered scans emit every signal.

Filter patterns: substring (`clk`), suffix glob (`*_valid`), prefix glob (`top.u_dma.*`).
`list` shows all aliases of matched signals, not only the matching paths.
A signal hit once may surface dozens of alias rows — use `--verbose` to group by `id`.
For one signal = one row, filter precisely and use `--verbose` — same `id` means same signal.


## Batch mode (one load, many queries)

For a pre-planned multi-step investigation of **one** file — especially a large
`.fsdb`/`.wlf` that is slow to open — use `--batch` to load the file once and run
a list of commands from stdin, instead of paying the open cost on every call:

```sh
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'search --condition valid=1,ready=1 --show data  #handshake' \
  | rwave --batch --json sim.fsdb
```

- One command per line — exactly what you'd type after `rwave`, minus the file
  (the file is given once on the `--batch` line). **Pass `--json`**: output is
  one NDJSON object per line, `{"id","ok","result"}` or `{"id","ok","error"}`,
  in input order.
- `id` is the trailing `#label` if present, else a 1-based line number. Correlate
  by **input order** (authoritative) or `id`. Blank and `#`-comment lines are
  skipped; `[global-opts]` on the `--batch` line are per-command defaults.
- Each `result` is byte-identical to the equivalent single-command `--json`
  output — parse it exactly the same way.
- A failing command is `"ok":false` and does **not** stop the batch; the process
  still exits `0`. Check each line's `ok`. Only a bad file or an unreadable
  stream is fatal (non-zero exit).
- Plan the full list up front — batch does not let you see one result before
  choosing the next. For adaptive, read-then-decide flows, use separate calls.


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

### Event-driven signal investigation

Use `search --condition --show` to bulk-extract field values across events —
one call replaces multiple `snapshot` calls. Catch both edges with
complementary `search --changed` (rising: `!=0`, falling: `=0`). Then drill
down with `compare` for jump deltas, `dump --limit 0` for full traces, and
`snapshot` for precise checkpoints.
When a transition is visible in a different signal's trace, use `dump --limit 0` +
external post-processing — not `search --changed`.

`dump` with multiple signals interleaves their events chronologically —
see e.g. a push flag and data bus transition side-by-side in one timeline.

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
- **Value format.** Multi-bit logic values print as `0x<hex>` (lower-case,
  leading zeros stripped — `0x4`, not `0x00000004`); 1-bit as `0`/`1`/`x`/`z`;
  a bus with any unknown bit as `b<bits>` (e.g. `b01x0`); real/string verbatim.
  Width is in the signal metadata, not the value — convert hex→int yourself if
  you need decimal.

For everything else (time syntax, filter syntax, value formatting, format
quirks, the FST `parameter`-value drop, performance notes) see the repo README.

