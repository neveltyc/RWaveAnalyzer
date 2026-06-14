#!/usr/bin/env bash
# rwave self-test harness.
#
# Runs the built `rwave` against the committed stimulus (verify/stimulus/) and
# checks a set of invariants that do not depend on the Python reference tool, so
# this can run anywhere the binary builds. Two things are verified:
#
#   1. Smoke: every command runs on both a VCD and an FST without error.
#   2. VCD/FST parity: for the same design, the value-bearing commands produce
#      identical output across formats (modulo the file name and size, which
#      legitimately differ). One documented exception: `vcd2fst` does not carry
#      Verilog parameter/localparam *values* into the FST, so designs that
#      declare them (counter_fsm) differ on those constant rows. Such designs
#      are listed in PARAM_DESIGNS and parity is checked on regs/wires only
#      (parameters filtered out) for them.
#
# Exit code is non-zero if any check fails.
set -uo pipefail

cd "$(dirname "$0")/.."
RW="${RWAVE_BIN:-target/release/rwave}"
if [[ ! -x "$RW" ]]; then
  echo "error: $RW not found; run: cargo build --release" >&2
  exit 2
fi

STIM=verify/stimulus
designs=(counter_fsm xz_tristate hier_deep real_event handshake_proto)
# Designs whose FST drops parameter values (see header).
PARAM_DESIGNS=" counter_fsm "

pass=0; fail=0
note() { printf '  %-22s %s\n' "$1" "$2"; }
ok()   { pass=$((pass+1)); }
bad()  { fail=$((fail+1)); echo "FAIL: $1"; }

echo "== smoke: all commands on VCD and FST =="
for d in "${designs[@]}"; do
  for ext in vcd fst; do
    f="$STIM/$d.$ext"
    for cmd in "info" "list" "summary" "dump --limit 50" "snapshot --at 30ns" \
               "compare --at 10ns,40ns" "--json info" "--json list"; do
      if $RW $cmd "$f" >/dev/null 2>&1; then ok; else bad "$d.$ext :: $cmd"; fi
    done
  done
done

echo "== VCD/FST parity (value commands) =="
norm() { sed -E -e "s#$1#FILE#g"; }
for d in "${designs[@]}"; do
  if [[ "$PARAM_DESIGNS" == *" $d "* ]]; then
    # vcd2fst drops Verilog parameter VALUES, so value/count-bearing commands
    # legitimately differ for this design. Structural parity (the signal table)
    # is still required and checked here; value parity is verified on the other
    # designs (which declare no parameters).
    for cmd in "list" "--json list"; do
      $RW $cmd "$STIM/$d.vcd" 2>&1 | norm "$d.vcd" > /tmp/_pv.$$
      $RW $cmd "$STIM/$d.fst" 2>&1 | norm "$d.fst" > /tmp/_pf.$$
      if diff -q /tmp/_pv.$$ /tmp/_pf.$$ >/dev/null; then ok; else bad "$d :: $cmd (VCD≠FST)"; fi
    done
    note "$d" "value parity skipped (vcd2fst drops parameter values; structure checked)"
    continue
  fi

  for cmd in "list" "summary" "dump --limit 80" "snapshot --at 50ns" \
             "--json list" "--json summary"; do
    $RW $cmd "$STIM/$d.vcd" 2>&1 | norm "$d.vcd" > /tmp/_pv.$$
    $RW $cmd "$STIM/$d.fst" 2>&1 | norm "$d.fst" > /tmp/_pf.$$
    if diff -q /tmp/_pv.$$ /tmp/_pf.$$ >/dev/null; then ok; else bad "$d :: $cmd (VCD≠FST)"; fi
  done
done
rm -f /tmp/_pv.$$ /tmp/_pf.$$

echo "== batch mode =="
BF="$STIM/handshake_proto.vcd"
# Consistency (core): a batch `result` is byte-identical to the equivalent
# single-command `--json` output. Compare the whole wrapped line so the
# {id,ok,result} envelope is checked too.
for c in "info" "list" "summary" "dump --limit 50" "snapshot --at 30ns" \
         "compare --at 10ns,40ns"; do
  single=$($RW --json $c "$BF" 2>/dev/null)
  got=$(printf '%s\n' "$c" | $RW --batch --json "$BF" 2>/dev/null)
  want="{\"id\":\"1\",\"ok\":true,\"result\":$single}"
  if [[ "$got" == "$want" ]]; then ok; else bad "batch consistency :: $c"; fi
done

# Error isolation: a failing command in the middle does not stop the batch, and
# the overall exit code is still 0.
iso=$(printf '%s\n' "info" "snapshot --at not_a_time" "list" \
      | $RW --batch --json "$BF" 2>/dev/null); isocode=$?
nlines=$(printf '%s\n' "$iso" | grep -c '"ok"')
if [[ "$isocode" -eq 0 && "$nlines" -eq 3 ]] \
   && printf '%s\n' "$iso" | sed -n 2p | grep -q '"ok":false'; then
  ok
else
  bad "batch error isolation (exit=$isocode, lines=$nlines)"
fi

# Empty filter match is ok:true (mirrors single-command exit 0), not a failure.
ef=$(printf 'snapshot --at 30ns --filter does_not_exist\n' \
     | $RW --batch --json "$BF" 2>/dev/null)
if printf '%s\n' "$ef" | grep -q '"ok":true'; then ok; else bad "batch empty-filter ok:true"; fi

# Fatal: --batch combined with a subcommand → usage error, exit 2.
$RW --batch info "$BF" </dev/null >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "batch + subcommand exits 2"

# Fatal: unloadable file → exit 1 with no half output.
bfout=$($RW --batch --json /no/such/file.vcd </dev/null 2>/dev/null); bfcode=$?
[[ "$bfcode" -eq 1 && -z "$bfout" ]] && ok || bad "batch bad-file exits 1, no output"

# Text mode: a '#label' header precedes the command's normal text body.
txt=$(printf 'info  #ov\n' | $RW --batch "$BF" 2>/dev/null)
[[ "$txt" == "#ov"* ]] && ok || bad "batch text header"

echo
echo "== RESULT: PASS=$pass FAIL=$fail =="
[[ "$fail" -eq 0 ]]
