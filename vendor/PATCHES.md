# Local patches to vendored code

Changes applied to vendored sources after import. Re-apply (or re-check) each
entry when updating a vendor copy.

## wellen (`vendor/wellen`, upstream ekiwi/wellen v0.23.0)

### 1. `$dumpall` is a checkpoint, not timestep zero

- **File:** `vendor/wellen/src/vcd.rs`, `parse_first_token` — the body-keyword
  `match token` block (one hunk).
- **Change:** upstream maps `b"$dumpall"` to `FirstTokenResult::Time(0)`; the
  patch moves it into the `IgnoredCmd` arm next to `$dumpvars`/`$dumpon`/
  `$dumpoff`.
- **Why:** per IEEE 1364-2005 §18.1.4/§18.2.3.9, `$dumpall` dumps *current*
  values at the *current* time — it introduces no timestep. Treating it as
  time 0 made any mid-file checkpoint parse as time flowing backwards: the
  wavemem encoder printed `WARN: time decreased …` and dropped the whole
  checkpoint (`skipping_time_step`), then later redundant re-statements of the
  lost values registered as transitions at wrong times, and pulses whose
  leading edge sat in the checkpoint vanished entirely.
- **Safety:** files that open with a dump block before any `#` timestamp are
  unaffected — `VcdEncoder::value` already synthesizes time 0 for values seen
  before the first timestep.
- **Upstream:** not fixed as of ekiwi/wellen main, 2026-07-18.
