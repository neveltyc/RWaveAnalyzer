---
name: waveform-debug
description: RTL waveform analysis CLI for debug, CI, and AI agents. Natively reads VCD, FST (preferred — ~10× smaller), and GHW. On linux-amd64, experimental support for WLF and FSDB via each vendor's own reader library. Use when the user has a waveform file (.vcd, .fst, .ghw, .wlf, .fsdb) and wants to inspect, search, compare, or summarize signals, browse the design hierarchy, or find what drives a signal — triggers on any mention of waveform analysis, signal queries, RTL debug, simulation results, or VCD/FST/WLF/FSDB files.
---

# rwave manual

`rwave` queries RTL simulation waveforms from the terminal: VCD, FST and GHW
everywhere, WLF and FSDB on linux-amd64. `rwave --help` is the syntax summary;
this manual (also `rwave --help --verbose`) is the full usage guide.

## Principles

1. **Always pass `--json`.** Text output is for humans.
2. **Orient before you query.** `info` → `tree`/`list` → a targeted command.
3. **Stay narrow on big files.** Give every `dump` a window and a selection;
   give `summary`, `snapshot` and `compare` a selection.
4. **Read `matched` and `hint` before the rows.** They say what the selection
   caught, why a result is empty, and when a result was cut.
5. **Never treat a truncated result as the whole answer.** On `truncated:true`,
   narrow the selection first; raise `--limit` only if every row is wanted.
6. **One `search --show` beats many `snapshot`s.** It returns the shown values
   across every span where the condition holds, in one call.
7. **Batch planned work, not adaptive work.** `--batch` saves reloading a large
   file, but you cannot see one result before writing the next line.
8. **Fix the command on exit 2; fix the environment on exit 1.** Do not retry
   the same call.

## Install

Release binaries: `rwave-linux-amd64` (the only one with WLF/FSDB),
`rwave-linux-arm64`, `rwave-windows-amd64.exe`, `rwave-macos-arm64`, plus
`sha256sums.txt`.

```bash
curl -fsSL -o ~/.local/bin/rwave \
  https://github.com/neveltyc/RWaveAnalyzer/releases/latest/download/rwave-linux-amd64
chmod +x ~/.local/bin/rwave
```

## Pick the command

```
What the user wants                     Command
─────────────────────────────────────── ────────────────────────────────────────
What's in this file?                    info
Which signals exist / match X?          list
Where am I in the hierarchy?            tree  (tree --of SIG: its ancestors)
What happened between T1 and T2?        dump
Which signals are active / static?      summary
What is X at time T?                    snapshot
What changed between T1 and T2?         compare
When does condition C hold?             search                  → interval rows
  ...and what were these values then?   search --show K1,K2     → segment rows
When does SIG transition?               search "changed(SIG)"   → event rows
Who drives / reads this signal?         trace (FSDB + built-in NPI only)
```

A segment row is a span where the condition holds and every shown value is
constant, so the row count depends on `--show`. To count transactions, use
event mode on the clock: `changed(clk),clk!=0,valid=1,ready=1`.

## Commands

| Command | Invocation | Fields to read |
|:--|:--|:--|
| `info` | `info <F>` | `signal_count`, `time_min_ticks`, `time_max_ticks`, `duration_h`, `timescale`, `timescale_assumed`, `scopes[]`, `var_types{}` |
| `list` | `list <F> [sel]` | `signals[].path`, `.width`, `.type`, `.id` (`--verbose`) |
| `dump` | `dump <F> --begin T --end T [sel]` | `window{}`, `matched`, `events[].time_ticks`, `.time_h`, `.path`, `.value` |
| `summary` | `summary <F> [--begin T] [--end T] [sel]` | `matched`, `active`, `static`, `unknown`, `rows[].path`, `.kind`, `.changes`, `.rise_count`, `.fall_count`, `.init`, `.last`, `.unique` (count), `.unknown` |
| `snapshot` | `snapshot <F> --at T [sel]` | `at_ticks`, `matched`, `known`, `undefined`, `signals[].path`, `.value` |
| `compare` | `compare <F> --at T1,T2 [sel]` | `matched`, `unchanged`, `diffs[].path`, `.at_t1`, `.at_t2` |
| `search` | `search <F> --condition C [--show K] [--begin T] [--end T] [sel]` | `mode`, `condition_resolved`, `rows[].begin_ticks`, `.end_ticks`, `.values{}` |
| `tree` | `tree <F> [SCOPE] [--depth N]` or `tree <F> --of SIG` | `mode`, `scopes[].path`, `.level`, `.signals`, `.children` |
| `trace` | `trace <F> SIG [--load] [--at T] [--control]` | `status`, `dir`, `endpoints[].statement`, `.file`, `.line`, `.signals[].path`, `.value` |

- Every result except `info` also has `command`, `shown`, `truncated`,
  `total`, `total_is_exact`, `hint`.
- `search` answers under `rows` in every mode; `mode` says how to read a row.
- `tree` defaults to depth 1 and counts scopes, not signals.

## Times and windows

- `fs` `ps` `ns` `us` `ms` `s` suffix (`17.5us`); a bare integer is raw ticks.
- `--begin`/`--end` (`dump`, `summary`, `search`) are inclusive.
- `--at T` for `snapshot`, `--at T1,T2` for `compare`, optional `--at T` for
  `trace`.

## Selecting signals

| Option | Effect |
|:--|:--|
| `--scope P1,P2` | Only these subtrees. Segment-aligned: `u_fifo` never matches `u_fifo_ctrl`. A path matches as a suffix: `u_tx.u_fifo` finds it anywhere. |
| `--depth N` | At most N levels below the scope; a direct signal is 1. Needs `--scope` (not for `tree`). |
| `--filter K1,K2` | Keep matching signals. |
| `--exclude K1,K2` | Drop matching signals; applied last. |
| `--exact` | A wildcard-free pattern never falls back to a substring. |

Pattern rules (also for `search`'s `--condition` and `--show` names):

| Pattern | Matches |
|:--|:--|
| `req` (no separator) | Leaf name only, so the signal and not the `u_sync_req` instance |
| `u_dma.` or `u_dma/` | Path; `.` and `/` are interchangeable |
| A signal's whole name | Only that signal: `state` → `state[2:0]`, not `state_next`; `u_dma.req` → `top.u_dma.req` |
| A name no signal has | Substring: `fifo` → every `*fifo*` |
| `name*`, `*data`, `a?b` | Glob anchored at both ends; bus range optional, so `*data` finds `data[7:0]` |
| `a,b` | Either; case-insensitive throughout |
| `''` | Option not given (lifts a batch default) |

- `[` and `]` are literal, so `data[7:0]` works as written.
- `--exclude clk` drops only signals named `clk`; drop every clock with `'*clk*'`.
- `search` resolves names inside the selection; a full path (bus range
  optional) bypasses it.
- Over-matched? Narrow with `--scope`/`--depth`, subtract with `--exclude`,
  or use the full name.

## Search conditions

| Form | Meaning |
|:--|:--|
| `SIG=VAL`, `SIG!=VAL` | Level test |
| `changed(SIG)` | True at each tick SIG transitions (not at t=0); switches to event mode |
| `A=1,B=0` | AND |
| repeated `--condition` | OR of the clauses; all have a `changed()` term or none does |

| Value | Example | Note |
|:--|:--|:--|
| Decimal | `cnt=5`, `cnt=-1` | Negative is two's complement at the signal width |
| Hex | `addr=0xff` | |
| Binary / 4-state | `st=b1010`, `bus=b1x0z` | `0b` prefix also accepted |
| Don't-care | `status=b?????1??` | `?` matches any bit; a short literal pads with 0, so write every bit |
| Unknown | `bus=x`, `bus=z` | Every bit x (or z); a partial or mixed unknown does not match |
| Real | `temp=-1.5`, `dac=100` | Numeric compare |
| String | `s=5` | A number or bit literal, compared as text; other text is rejected |

- A condition name must resolve to exactly one signal; an ambiguous one errors
  with candidates. A `--show` pattern may match several.
- A value the signal cannot hold is an error (exit 2), not an empty result:
  `state=99` on 3 bits, `temp=b101` on a real, a level test on an event.
- `!=` never matches an x/z bit in a cared position.
- Rising edge `changed(x),x!=0`; falling edge `changed(x),x=0`.
- `changed(a),changed(b)` means both on the same tick.
- No `|` or parentheses; use repeated `--condition` for OR.
- Double-quote conditions; single-quote names containing `$`.

## Reading JSON

| Value | Format | Parse |
|:--|:--|:--|
| 1-bit logic | `0`, `1` | `int(v, 16)` |
| Multi-bit logic | `0x4` (no zero padding) | `int(v, 16)` |
| Any x/z bit | `bx`, `bz`, `b01x0` (full width) | `v.startswith('b')`; bits are `v[1:]` |
| Real | `1.5`, `NaN` | `float(v)` |
| String | as recorded | |
| Event | `triggered` | |

- Pick the parser from the signal's `type` (`list`): `real`, `string` and
  `event` are not logic. Never test `'x' in v`: every `0x` value has an `x`.
- Every documented key is always present; empty is `null`, `0`, `[]` or `{}`.
  Test values, not presence.
- `matched` is `null` without a selection option; else `matched.count` is how
  many signals it caught.
- Errors are `{"command","ok":false,"error"}` on stderr; stdout carries results
  only.

| Exit | Meaning | Do |
|:--|:--|:--|
| 0 | Answered, an empty result too | Read `hint` when empty |
| 1 | File or environment: open/parse, design library, license, unwritable result | Fix the setup |
| 2 | Request: flags, names, conditions, times, the `-o` target | Fix the command |
| 3 | Terminal output cut at 1 MiB | Use `-o FILE` or a pipe |

## Large results

- `--limit N` caps rows (default 500, `0` unlimited). With `-o` or
  `--verbose` and no `--limit`, nothing is cut, so keep a selection on large files.
- `-o FILE` writes the whole result to FILE and prints a one-line receipt.
  Read a large file in pieces.
  Keep a selection on: an unfiltered query on a big file is big on disk too.
- With `--batch`, `-o` goes on the `--batch` line and covers the run.

## Batch mode

```bash
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'search --condition valid=1,ready=1 --show data  #handshake' \
  | rwave --batch --json sim.fsdb
```

- One command per line, without `rwave` and the file.
- Blank and `#` lines are skipped; a trailing `#label` becomes the `id`.
- Options on the `--batch` line are per-line defaults; a line overrides one or
  lifts it with `''`.
- Output is one NDJSON line per command, in input order:
  `{"id","ok","result"}` or `{"id","ok":false,"error","exit"}`.
- `result` is identical to the single-command `--json` output.
- A failed line does not stop the run, so check every `ok`.

## Vendor formats (linux-amd64, experimental)

```bash
export VERDI_HOME=/path/to/verdi                            # .fsdb, needs Verdi-Ultra
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so # .wlf
```

| Variable | Use |
|:--|:--|
| `VERDI_HOME` | Verdi install for `.fsdb` |
| `RWAVE_WLF_LIB` | `libwlf.so` for `.wlf` |
| `RWAVE_FSDB_LIB`, `RWAVE_NPI_L1_LIB` | `libNPI.so` / `libnpiL1.so` in a non-standard layout |
| `RWAVE_PLUGIN_FSDB` | External FSDB plugin; overrides the built-in backend, no `trace` |
| `RWAVE_PLUGIN_<EXT>` | External backend for any other extension |
| `RWAVE_TRACE_EN=0` | Disable `trace` |

If the library, license or platform is missing, the command exits 1 with the
reason. Suggest converting the dump to FST or VCD instead.

## trace

- Works only on `.fsdb` opened through the built-in NPI backend; elsewhere it
  exits 2. Do not retry on another format.
- Pass `--kdb` only when rwave asks for it, and only with a path the user
  gives. The design library path is read from the FSDB.
- Read `status` first: `resolved`, `boundary_only` (driver outside the traced
  hierarchy), `no_driver_found`.
- Clocks, resets and enclosing `if`/`case` are left out unless `--control`.
- Several drivers usually means a reset arm and a data arm. Loads are often
  many, so pass `--limit`.
- With `--at T`, each endpoint's `signals[]` carry their values. Follow a
  `signals[].path` into the next `trace` to walk the cone.

## Workflows

All with `--json`.

**First look**

```
info                              time range, timescale, scopes
tree --depth 2                    top of the hierarchy
list --scope <block> --depth 1    one block's own signals
summary --scope <block>           active vs static, unknown bits
```

**At time T**

```
snapshot --at T --filter <sigs>
dump --begin T-Δ --end T+Δ --filter <sigs>
compare --at T-Δ,T+Δ --scope <block>
```

**Protocol transactions**

```
list --filter '*valid,*ready,*addr,*data'
search --condition "arvalid=1,arready=1" --show araddr,arlen
search --condition "ch0_valid=1,ch0_ready=1" --condition "ch1_valid=1,ch1_ready=1"
```

**Unknown values**

```
summary --scope <block>                    rows with unknown:true had an x/z bit
search --condition "state=x"               when every bit is x
dump --filter <sig> --begin .. --end ..    partial x/z values
snapshot --at <first hit> --scope <block>  context at that moment
```

**Clock and reset**

```
summary --filter '*clk*,*rst*,*reset*'
```

A clock should show balanced `rise_count`/`fall_count`; a reset should be
static after its first release.

**Edges and events**

```
search --condition "changed(req),req!=0" --show addr     rising edges, with addr
search --condition "changed(req),ready=0"                req toggles while not ready
```

For a transition seen only through another signal's trace, use
`dump --limit 0 -o FILE` and post-process the file.

**Why does X have this value?** (FSDB + NPI)

```
snapshot --at T --filter <sig>
trace <sig> --at T                driving statement, file:line, input values
trace <driver input> --at T       one level up
trace <sig> --load                who reads it
```

## Other behavior

- A flag the command does not read is an error: `dump --at`, `list --begin`,
  `info --filter`.
