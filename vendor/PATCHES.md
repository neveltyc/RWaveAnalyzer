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
- **Upstream:** reported as [ekiwi/wellen#137][w137] and **fixed upstream in
  v0.25.6** (issue closed 2026-07-21). Upstream's fix is the same edit this
  patch makes — `$dumpall` moved into the `IgnoredCmd` arm — so the two are
  behaviourally identical, and upstream additionally carries an Icarus-generated
  regression VCD.
- **On next sync: drop this patch.** It is redundant at wellen >= 0.25.6; the
  vendored copy is pinned at 0.23.0, which predates the fix, so it is still
  required until that bump happens. Re-applying it on top of >= 0.25.6 would
  not break anything (the arm is already correct) but the hunk will not apply.

[w137]: https://github.com/ekiwi/wellen/issues/137

### 2. Malformed values read as x / NaN instead of panicking

- **Files:** `vendor/wellen/src/vcd.rs` `decode_vcd_bit_vec_change`;
  `vendor/wellen/src/wavemem.rs` (real and string encoding).
- **Change:** on a logic id, an invalid character (a real `rnan`/`r1` written
  to it) or a 1/x/z beyond the declared width reads as x (all-`x` for a
  vector); extra leading zeros are dropped (`b01` on 1 bit is 1); an empty
  body (`b !`) is 0. A real that does not parse is NaN; a string value
  without its `s` prefix is kept as written. Upstream panics on each (or,
  for a 1-bit id, reads the last character).
- **On next sync:** re-apply; upstream still panics.

### 3. UTF-8 BOM before the header

- **File:** `vendor/wellen/src/vcd.rs`, `read_command`.
- **Change:** a leading `EF BB BF` is skipped; upstream rejects the file as
  not a VCD.
- **On next sync:** re-apply.

### 4. Parse-time warnings go to stderr

- **Files:** `vendor/wellen/src/{stream.rs,wavemem.rs,fst.rs}`.
- **Change:** `println!` → `eprintln!` for the "time decreased" warning and
  the FST VHDL type notes; on stdout they broke `--json` output. The
  statistics printers stay on stdout (rwave never calls them).
- **On next sync:** re-apply.
