# rwave JSON output schema

Every command takes `--json` and answers with one compact JSON object.

## The contract

**Shape follows the invocation, never the data.** For a given command and a
given set of flags, the key set is fixed: every key listed below is always
present, in the order shown. A key with nothing to say carries `null`, `0`,
`[]` or `{}` — it never disappears. So `--verbose` adds fields (you know
whether you passed it), and a row that happens to have no unique values still
carries `unique`.

Four rules follow from that:

- **Never test for a key's presence.** Test its value.
- **A key name means one type.** `*_ticks` is always an integer or `null`,
  `*_h` always a string or `null`, `hint` always a string or `null`.
- **A key name means one shape within a command.** Across commands the same
  name is free to mean the command's own thing: `list`'s `signals[]` describes
  declarations, `snapshot`'s describes values. You always know which command
  you ran.
- **The payload array is the last key**, and its name is fixed per command
  regardless of mode: `search` answers under `rows` in all three modes, `tree`
  under `scopes` in both, `trace` under `endpoints` in both directions.

## Time

One spelling, everywhere: a `<name>_ticks` integer and a `<name>_h` string.

```json
"begin_ticks": 3321600, "begin_h": "3.3216ms"
```

`_ticks` is the raw tick count in the file's timescale — compare and compute on
this. `_h` is for display. Both are `null` together when the time is absent.

Times parsed from `--begin`/`--end`/`--at` are echoed back, so a value that
rounded to a neighbouring tick, or landed outside the trace, is visible in the
answer rather than only in an empty result.

## Counts, truncation, and hints

Every command carries the same four, in this order:

| key | type | meaning |
|---|---|---|
| `shown` | int | rows in the payload array |
| `truncated` | bool | whether `--limit` clipped the result |
| `total` | int | how many there were |
| `total_is_exact` | bool | `false` when `total` is a lower bound — the command stopped counting once the limit was met |

`hint` is a string or `null`: a sentence about the result worth acting on —
that it was clipped and how to lift the cap, that the selection matched through
an alias, or why an empty result is empty. When there is more than one, they
are joined with `; `.

## Selection reporting

`dump`, `summary`, `snapshot` and `compare` carry:

```json
"matched": {"count": 2, "paths": ["tb.req", "tb.req_strobe"]},
"selected": 2
```

`matched` is `null` when no selection option was given. `count` is exact;
`paths` stops at 10, so `count > len(paths)` means the rest were not listed.
`paths` holds the names that **matched**, which is not always the name the rows
carry — a signal declared under several names is labelled with its canonical
path, and `hint` says so when the two differ.

`list` and `search` take the same options and carry neither key: `list`'s whole
output is the match list, and `search` selects through its `--condition` and
`--show` names rather than through rows.

## Empty results

An empty result says which kind of empty it is, in `hint`:

| cause | what to do |
|---|---|
| `the selection matched no signals` | fix the pattern |
| `carries no recorded data anywhere in the file` | the signal is in the hierarchy but was never dumped — no query will help |
| `the window begins at X, after the last event at Y` | fix the time |
| `no value changes in the window` | nothing happened; that is the answer |

The second and fourth can combine when only part of the selection was dumped.

## Errors

With `--json`, a failure is JSON too — on **stderr**, with a non-zero exit code
(2 for a usage error, 1 for a runtime one). stdout carries results only.

```json
{"command":"dump","ok":false,"error":"invalid time value 'banana'; ..."}
```

`command` is `null` for every usage error (exit 2), including one whose message
names the command: argument checking finishes before the command is handed on,
and the error is reported from there. It is set for runtime errors (exit 1).
A successful result has no `ok` key: the exit code already said so.

## Per-command keys

Every object below begins with `"command": "<name>"`. `V` marks a key that
appears only under `--verbose`.

### info

`file`, `size_bytes`, `timescale`, `date`, `version`, `comments[]`,
`signal_count`, `reference_count`, `synthesized_buses`, `var_types{}`,
`time_min_ticks`, `time_min_h`, `time_max_ticks`, `time_max_h`,
`duration_ticks`, `duration_h`, `scopes[]`

`var_types` is a map from type name to count, so its keys come from the file.
`comments` is always `[]` and `synthesized_buses` always `0`.

### list

`shown`, `truncated`, `total`, `total_is_exact`, `hint`, `signals[]`

`signals[]`: `path`, `width`, `type`, `id`&nbsp;*V*

One row per alias path, so a signal declared twice appears twice; under
`--verbose` the shared `id` identifies them as one signal.

### dump

`window{}`, `matched`, `selected`, `shown`, `truncated`, `total`,
`total_is_exact`, `hint`, `events[]`

`window{}`: `begin_ticks`, `begin_h`, `end_ticks`, `end_h` (the end is `null`
when `--end` was not given, meaning "to the end of the trace")

`events[]`: `time_ticks`, `time_h`, `path`, `value`, `width`&nbsp;*V*,
`type`&nbsp;*V*

`total_is_exact` is `false` when truncated: `dump` stops reading at the limit.

### summary

`window{}`, `matched`, `selected`, `defined`, `undefined`, `active`, `static`,
`unknown`, `shown`, `truncated`, `total`, `total_is_exact`, `hint`, `rows[]`

`rows[]`: `kind` (`active`/`static`/`undefined`), `path`, `value`, `changes`,
`rise_count`, `fall_count`, `init`, `last`, `first_at_ticks`, `first_at_h`,
`last_at_ticks`, `last_at_h`, `unique`, `unknown`, `width`&nbsp;*V*,
`type`&nbsp;*V*

`value` is the held value on a `static` row and `null` on an `active` one.
`rise_count`/`fall_count` are `null` for anything but a 1-bit signal. A static
row's `first_at_*`/`last_at_*` are `null`. `unique` counts distinct values in
the window: 1 for a static signal, 0 for an undefined one. `undefined` rows
appear only under `--verbose`.

### snapshot

`at_ticks`, `at_h`, `matched`, `selected`, `known`, `undefined`, `shown`,
`truncated`, `total`, `total_is_exact`, `hint`, `signals[]`

`signals[]`: `path`, `value`, `undefined`, `width`&nbsp;*V*, `type`&nbsp;*V*

`value` is `null` exactly when `undefined` is `true`. Undefined rows appear
only under `--verbose`; the top-level `undefined` counts them either way.

### compare

`t1_ticks`, `t1_h`, `t2_ticks`, `t2_h`, `matched`, `selected`, `unchanged`,
`shown`, `truncated`, `total`, `total_is_exact`, `hint`, `diffs[]`

`diffs[]`: `path`, `at_t1`, `at_t2`, `width`&nbsp;*V*, `type`&nbsp;*V*

Only differing signals are rows; `unchanged` counts the rest.

### search

`mode`, `condition`, `condition_resolved`, `changed[]`, `show[]`, `window{}`,
`shown`, `truncated`, `total`, `total_is_exact`, `hint`, `rows[]`

`rows[]`: `begin_ticks`, `begin_h`, `end_ticks`, `end_h`, `values{}`,
`meta{}`&nbsp;*V*

`mode` is `interval`, `segment` or `event` and says how to read a row, but not
which keys it has — all three share the row above.

- `interval`: a span where the condition held; `values` is `{}`.
- `segment`: the same span split where a `--show` value changed; `values` holds
  those values.
- `event`: an instant, so `end_ticks`/`end_h` are `null`; `values` holds the
  `--show` values at that tick.

`changed[]` lists the `changed()` signals and is `[]` outside event mode.
`values{}` is keyed by signal path. `meta{}` maps each shown path to
`{raw, width, type}`.

### tree

`mode`, `signal`, `roots[]`, `depth`, `root_signals`, `shown`, `truncated`,
`total`, `total_is_exact`, `hint`, `scopes[]`

`scopes[]`: `path`, `name`, `level`, `signals`, `children`

`mode` is `subtree` or `chain`. In `chain` mode (`--of`) `signal` names the
signal and `depth`/`root_signals` are `null`; in `subtree` mode `signal` is
`null`. Both answer under `scopes`.

### trace

`signal`, `dir`, `mode`, `kdb`, `status`, `at_ticks`, `at_h`,
`unresolved_in_wave`, `shown`, `truncated`, `total`, `total_is_exact`, `hint`,
`endpoints[]`

`endpoints[]`: `group`, `kind`, `npi_type`, `statement`, `scope`, `file`,
`line`, `boundary`, `signals[]`

`endpoints[].signals[]`: `path`, plus `value` when `--at` was given.

`dir` is `driver` or `load` and says what `endpoints` holds. `at_ticks`,
`at_h` and `unresolved_in_wave` are `null` when `--at` was not given, and the
endpoint signals carry no `value` in that case — the one place where a flag
adds a key to a nested row rather than a top-level one.

## `--exact` and vectors

`--exact` requires the whole leaf name, and a vector's leaf name includes its
range suffix as declared. `--filter state --exact` therefore matches nothing on
a bus called `state[2:0]`; write `--filter 'state[2:0]' --exact`, or drop
`--exact` and let the substring match do it. `[` and `]` are literal characters
in a pattern, so no quoting beyond the shell's is needed.

## Batch mode

`--batch --json` frames each result as one NDJSON line:

```json
{"id":"1","ok":true,"result":{ …the single-command object… }}
{"id":"2","ok":false,"error":"invalid command: 'bogus' (choose from …)"}
```

`result` is byte-identical to the equivalent single-command `--json` output,
`command` field included. Results come back in input order.

## Compatibility

This schema is the 0.3.0 contract. Changes to it are breaking changes and get a
minor version bump and a CHANGELOG entry; new keys may be added within a minor
version, so read the keys you need and ignore the rest.
