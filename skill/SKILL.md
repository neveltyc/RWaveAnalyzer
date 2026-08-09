---
name: waveform-debug
description: RTL waveform analysis CLI for debug, CI, and AI agents. Natively reads VCD, FST (preferred — ~10× smaller), and GHW. On linux-amd64, experimental support for WLF and FSDB via each vendor's own reader library. Use when the user has a waveform file (.vcd, .fst, .ghw, .wlf, .fsdb) and wants to inspect, search, compare, or summarize signals, browse the design hierarchy, or find what drives a signal — triggers on any mention of waveform analysis, signal queries, RTL debug, simulation results, or VCD/FST/WLF/FSDB files.
---

# rwave — agent skill

`rwave` is a single binary for querying RTL simulation waveforms from the
terminal. It natively reads **VCD**, **FST**, and **GHW** (prefer FST — typically
10× smaller than VCD). On linux-amd64 it also provides experimental support for
**WLF** (Questa/ModelSim) and **FSDB** (Verdi) by calling into each vendor's own
reader library interface. Nine commands cover inspection, search, comparison,
summary, hierarchy, and driver/load tracing. **Always pass `--json` from an
agent.** This file covers
what is unique to driving the tool from an agent — see the repo README for the
full reference.

## Install

Prebuilt binaries are attached to every tagged release, with one `sha256sums.txt`
covering them all (`sha256sum -c sha256sums.txt`).
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
# Also set LD_LIBRARY_PATH with the Verdi NPI, FsdbReader, and Qt5 lib dirs.
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
├─ "When does condition C hold?" / "Find handshakes"
│   └─ search         condition-based, three sub-modes:
│       ├─ interval   time ranges where condition is true (no --show, no changed())
│       ├─ segment    intervals + observed values         (with --show)
│       └─ event      fires when signals transition       (changed(SIG) in condition)
├─ "Where am I?" / "What's under tb.dut?"
│   └─ tree           hierarchy; --of SIGNAL gives its full ancestor chain
└─ "Who drives this signal?" / "What reads it?"
    └─ trace          drivers/loads with file:line; .fsdb (NPI) or .wlf (Questa)
```

`search`'s JSON top-level key depends on the mode: `intervals` /
`segments` / `events`. Always check `mode` before parsing.

## Condition syntax (search only)

Comma-separated AND list. Each item is `SIG=VAL`, `SIG==VAL`, `SIG!=VAL`,
or `changed(SIG)`.

- Signal pattern must resolve to **exactly one** signal. If ambiguous,
  the error lists candidates — add `--scope`/`--exclude` to narrow the
  lookup, or give the full path (a full path bypasses selection).
- Values: decimal (`5`), hex (`0xff`), binary (`b1010` / `0b1010`),
  4-state (`b1x0z`), or bare `x`/`z`.
- `!=` does **not** match `x`/`z` ("unknown is not evidence of
  difference"). To find unknowns, ask explicitly with `sig=x`.
- `changed(SIG)`: edge predicate, true at exactly the ticks where SIG
  transitions (t=0 initialization is not a transition). Its presence
  switches the search to event mode. One signal per changed();
  `changed(a),changed(b)` = both transition on the same tick. Level terms
  in the clause are evaluated on the post-update state at that tick.
  Rising edges only: `changed(x),x!=0`; falling only: `changed(x),x=0`.
  With no `--show`, event mode shows the changed() signals.
- OR: repeat `--condition`. Each `--condition` is one AND clause; the search
  holds wherever **any** clause holds (OR-of-ANDs) — e.g. one clause per
  channel for "any channel handshakes". Identical / term-reordered / alias-
  equivalent clauses fold silently. No in-string OR (`|` / parentheses).
  Every clause must contain a `changed()` term or none may (modes cannot mix).

## Command quick reference

`<F>` is the input file. See the repo README for the full surface; the table
below is the agent-side cheat sheet of the JSON-form arguments and the
fields you'll usually parse out.

| Command | Common invocation | Useful JSON fields |
|---|---|---|
| `info` | `rwave --json info <F>` | `signal_count`, `time_min_ticks`, `time_max_ticks`, `duration_h`, `timescale`, `scopes[]`, `var_types` |
| `list` | `rwave --json list <F> [selection]` | `signals[].path`, `signals[].width`, `signals[].type` |
| `dump` | `rwave --json dump <F> --begin T --end T [selection]` | `events[].time_ticks`, `events[].time_h`, `events[].path`, `events[].value` |
| `summary` | `rwave --json summary <F> [selection]` | `rows[].path`, `rows[].kind`, `rows[].changes`, `rows[].rise_count`/`fall_count`, `rows[].init`, `rows[].last`, `active`, `static` |
| `snapshot` | `rwave --json snapshot <F> --at T [selection]` | `signals[].path`, `signals[].value`, `at_ticks`, `at_h`, `known`, `undefined` |
| `compare` | `rwave --json compare <F> --at T1,T2 [selection]` | `diffs[].path`, `diffs[].at_t1`, `diffs[].at_t2`, `time1_ticks`, `time1_h`, `time2_ticks`, `time2_h` |
| `search` | see decision tree above | `mode`, then one of `intervals[]` / `segments[]` / `events[]` |
| `tree` | `rwave --json tree <F> [SCOPE] [--depth N]` | `mode` (`subtree`/`chain`), `roots[]`, `root_signals`, `scopes[]`/`chain[]` with `.path`, `.name`, `.level`, `.signals`, `.children` |
| `trace` | `rwave --json trace <F> <SIG> [--load] [--at T] [--control]` | `status`, `drivers[]`/`loads[]` with `.kind`, `.raw_kind`, `.statement`, `.file`, `.line`, `.boundary`, `.signals[].path`/`.value` |

`tree` works on every format. `trace` needs a design database beside the
waveform: `.fsdb` with Verdi's KDB, or `.wlf` with Questa's `.dbg` (and `vsim`
reachable). Elsewhere it exits 1. That is a capability limit, not a transient
failure, so do not retry it on another file format.

`trace` rules:

- Never pass `--kdb` unless rwave asks for it. The design library is read from
  the FSDB header; if that fails, use only a path the user supplies. On a `.wlf`
  it is refused outright — Questa finds its own `.dbg` by basename.
- On a `.wlf`: `--control` is refused (Questa records no gating dependencies) and
  `--load` gives the reading processes without `file:line`, which is all Questa
  keeps after simulation. Batch many traces in one `--batch` run: each rwave
  process starts one `vsim` and holds two Questa licences until it exits.
- Read `status` before trusting the list: `resolved`, `boundary_only` (the
  driver is outside the traced hierarchy), `no_driver_found`.
- Clock, reset, and enclosing `if`/`case` are excluded unless you pass
  `--control`.
- One driver is the normal answer; several usually means a reset arm plus a
  data arm. Loads are often many, so pass `--limit`.

For `dump`, **always pass `--begin/--end` and a selection** — running it
unbounded on a large dump streams the whole file.
For `snapshot` and `compare` on large files, **always pass a selection** — unfiltered scans emit every signal.

## Selecting signals

Four options, applied to each signal path in turn. They work on every command
except `info` — `list`, `dump`, `summary`, `snapshot`, `compare`, and `search`
(the last narrows name resolution rather than rows, see below) — and all work
as `--batch` defaults. `info` describes the file and ignores them.

| | |
|---|---|
| `--scope P1,P2` | restrict to subtrees |
| `--depth N` | at most N levels below the `--scope` root; a signal directly in it is depth 1. Requires `--scope` |
| `--filter K1,K2` | keep matching signals |
| `--exclude K1,K2` | drop matching signals; applied last |

**A pattern with no separator matches the leaf name; one containing a `.` or
`/` matches the whole path.** This is the rule to internalize. RTL names scopes after signals —
a CDC synchronizer instance is conventionally `u_sync_<sig>` — so `--filter
tx_fifo_push_err` gets you the status bit, not the synchronizer's clocks and
flops. When you *do* want a subtree, write the separator: `--filter 'u_dma.'`, or
`--exclude 'u_sync_status.'` to drop one. `--exclude u_sync_status` (bare)
drops nothing, because no *leaf* is called that.

`--scope` matches segment-wise, so `u_fifo` never selects `u_fifo_ctrl`. A bare
value names an instance (`*`/`?` allowed) and includes its descendants; a path
value is a segment-aligned suffix, so `u_tx.u_fifo` finds that subtree without
you knowing the path from the root.

If `list --filter X` returns far more rows than expected, **do not just raise
`--limit`** — narrow structurally with `--scope`/`--depth`, or subtract with
`--exclude`.

Selection is per path: a signal is kept when any one of its paths clears every
option, so excluding a synchronizer never costs you the status bit wired into
it. `list` prints only the paths that survived `--scope`/`--depth`/`--exclude`;
`--filter` hides no rows, so a hit may still surface several alias rows — use
`--verbose` and group by `id` (same `id` = same signal).

`search` has no row filter (its `--condition`/`--show` names are the
selection), so the options narrow **name resolution** instead: they are usually
what turns `pattern ... matches N signals` into a unique hit. A name written as
a full path bypasses selection entirely. Note for batch plans: a `--batch`-line
`--filter` now narrows `search` lines too; pass `--filter ''` on a line to lift
it.

A selection matching nothing is an empty result with `ok:true`, not an error.


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
2. list --scope <block>        see one block at a time (--depth 1 to skip submodules)
3. list --filter <suspect>     find the signals of interest by name
4. summary --filter <window>   spot active vs static signals
5. dump or search              drill into specifics
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

### Orient in an unfamiliar hierarchy

```
1. tree                                  top-level scopes
2. tree <scope> --depth 2                what is inside one block
3. tree --of <signal>                    the ancestor chain of a signal you already have
```

`tree --of` is the fastest way back up from a deep leaf: every row is a path you
can paste straight into `--scope`. `--depth` counts scopes here, where `list`
counts signals.

### "Why does this signal have that value?" (FSDB + NPI only)

```
1. snapshot --at T --filter <signal>     confirm the value
2. trace <signal> --at T                 the driving statement, with file:line
3. trace <driver_signal> --at T          walk one level up the cone
4. trace <signal> --load                 or go the other way: who reads it
```

Each `drivers[]` entry carries `statement`, `file`, `line`, and the `signals[]`
it reads; with `--at` those come with their values, so one call answers both
"who drives it" and "what was it carrying". Follow a `signals[].path` into the
next `trace` to walk the cone. Stop when `status` is `boundary_only`, which
means the driver is outside what could be followed, or when a hop reaches a
primary input.

### Clock/reset sanity

```
summary --filter clk,rst,reset
# clk should toggle with balanced rise/fall
# rst should be static after the initial assertion
# noisy? --exclude '*_clkgen.*' drops generated clock trees
```

### Event-driven signal investigation

Use `search --condition --show` to bulk-extract field values across events —
one call replaces multiple `snapshot` calls. Catch specific edges with
`changed()` terms (rising: `changed(x),x!=0`; falling: `changed(x),x=0`).
Then drill down with `compare` for jump deltas, `dump --limit 0` for full
traces, and `snapshot` for precise checkpoints.
When a transition is visible in a different signal's trace, use `dump --limit 0` +
external post-processing — not `search` with `changed()`.

`dump` with multiple signals interleaves their events chronologically —
see e.g. a push flag and data bus transition side-by-side in one timeline.

## Agent-side gotchas

- **Output truncation.** Default `--limit` is 500. A clipped result carries
  `truncated: true` and a `hint` field spelling out the re-run — take it,
  with `--limit 0` (unlimited) or a larger value. `total_is_exact: false`
  means `total` is a lower bound, not the true count. Never treat a
  truncated result as the whole answer.
- **`search` mode discriminator.** The output's top-level array key
  depends on the mode (`intervals` / `segments` / `events`). Always read
  the `mode` field first.
- **Exit code is non-zero on errors.** Errors are a single line on stderr
  starting with `Error:`. Catch and parse them.
- **`--json` everywhere.** Mixing text-mode parsing in is the most common
  source of fragility. Pass `--json` on every invocation.
- **Shell quoting.** Double-quote conditions (`--condition "changed(req),ready=0"`
  — parens are shell metacharacters). If a signal name contains `$` (common
  in gate-level netlists), single-quote so the shell doesn't expand it.

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

