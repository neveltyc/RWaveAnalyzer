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

# `search` needs signal names, so it cannot ride the all-designs loops above.
# handshake_proto exists to exercise it: valid/ready/data cover all three modes
# (interval, segment, event). Both that the mode runs and that VCD and FST agree.
echo "== search modes (VCD/FST parity) =="
SD=handshake_proto
for cmd in "search --condition valid=1,ready=1" \
           "search --condition valid=1,ready=1 --show data" \
           "search --condition changed(data)" \
           "search --condition changed(data),valid=1,ready=1" \
           "--json search --condition changed(data)"; do
  $RW $cmd "$STIM/$SD.vcd" > /tmp/_sv.$$ 2>&1; rcv=$?
  $RW $cmd "$STIM/$SD.fst" > /tmp/_sf.$$ 2>&1; rcf=$?
  if [[ "$rcv" -eq 0 && "$rcf" -eq 0 ]] && diff -q /tmp/_sv.$$ /tmp/_sf.$$ >/dev/null; then
    ok
  else
    bad "$SD :: $cmd (rc=$rcv/$rcf, VCD≠FST?)"
  fi
done

# Mode discriminator: the top-level key follows the condition, not a flag.
if $RW --json search "$STIM/$SD.vcd" --condition 'changed(data)' 2>/dev/null \
     | grep -q '"mode":"event"'; then ok; else bad "changed() selects event mode"; fi
if $RW --json search "$STIM/$SD.vcd" --condition 'valid=1' 2>/dev/null \
     | grep -q '"mode":"interval"'; then ok; else bad "level-only stays interval mode"; fi

# Mixing edge and level clauses has no single row shape. It is caught in setup
# (resolving terms needs the file), so it is a runtime error — exit 1, not the
# exit 2 of an argv usage error.
mix=$($RW search "$STIM/$SD.vcd" --condition 'changed(data)' --condition 'valid=1' 2>&1)
mixcode=$?
[[ "$mixcode" -eq 1 && "$mix" == *"cannot mix changed()"* ]] \
  && ok || bad "mixed changed()/level clauses (exit=$mixcode)"

# The removed --changed flag is an argv error (exit 2) that names the
# replacement syntax. Captured, not piped: `pipefail` would report rwave's
# non-zero exit for the whole pipeline even when the grep matched.
cf=$($RW search "$STIM/$SD.vcd" --condition 'valid=1' --changed data 2>&1)
cfcode=$?
[[ "$cfcode" -eq 2 && "$cf" == *'changed(SIG)'* ]] \
  && ok || bad "--changed hints at changed(SIG) (exit=$cfcode)"

rm -f /tmp/_sv.$$ /tmp/_sf.$$

echo "== selection (--scope / --depth / --filter / --exclude) =="
# hier_deep is three levels deep with repeated instance names, so it exercises
# every axis: root -> u_m0,u_m1 -> u_a,u_b.
HD="$STIM/hier_deep"
rowcount() { grep -cE '^  [a-z]' ; }

# VCD/FST parity under selection: the options must narrow both formats the same
# way, since they read the hierarchy through different backends.
for sel in "--scope u_m0" "--scope u_m0.u_a" "--scope u_m0 --depth 1" \
           "--filter cnt --exclude u_m1." "--json list --scope u_m1 --depth 1"; do
  case "$sel" in
    --json*) v=$($RW $sel "$HD.vcd" 2>&1); f=$($RW $sel "$HD.fst" 2>&1) ;;
    *)       v=$($RW list "$HD.vcd" $sel 2>&1); f=$($RW list "$HD.fst" $sel 2>&1) ;;
  esac
  v=$(printf '%s' "$v" | norm "$HD.vcd"); f=$(printf '%s' "$f" | norm "$HD.fst")
  if [[ "$v" == "$f" ]]; then ok; else bad "selection VCD/FST parity :: $sel"; fi
done

# --scope selects the subtree and its descendants; --depth cuts at the root.
n=$($RW list "$HD.vcd" --scope u_m0 | rowcount)
[[ "$n" -eq 10 ]] && ok || bad "--scope u_m0 rows (got $n, want 10)"
n=$($RW list "$HD.vcd" --scope u_m0 --depth 1 | rowcount)
[[ "$n" -eq 4 ]] && ok || bad "--scope u_m0 --depth 1 rows (got $n, want 4)"
# Segment-aligned: a dotted value is a suffix of the scope path.
n=$($RW list "$HD.vcd" --scope u_m0.u_a | rowcount)
[[ "$n" -eq 3 ]] && ok || bad "--scope u_m0.u_a rows (got $n, want 3)"

# A dot-free --filter matches leaf names, so a scope name selects nothing.
n=$($RW list "$HD.vcd" --filter u_m0 | rowcount)
[[ "$n" -eq 0 ]] && ok || bad "--filter on a scope name matches no leaf (got $n)"
n=$($RW list "$HD.vcd" --filter u_m0. | rowcount)
[[ "$n" -gt 0 ]] && ok || bad "dotted --filter still matches the path"

# Escaped identifiers keep their dots: the leaf of `tb.\foo.bar` is `\foo.bar`,
# so the name matches and the scope does not. Splitting on the last separator
# would answer "bar" and lose half the name.
EF=verify/fixtures/escaped_trace.vcd
n=$($RW list "$EF" --filter bar | rowcount)
[[ "$n" -eq 1 ]] && ok || bad "escaped identifier matched by its leaf (got $n)"
n=$($RW list "$EF" --filter tb | rowcount)
[[ "$n" -eq 0 ]] && ok || bad "escaped identifier not matched by its scope (got $n)"

# --depth is measured from the --scope root, so it is a usage error without one.
$RW list "$HD.vcd" --depth 1 >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "--depth without --scope exits 2"
$RW list "$HD.vcd" --scope u_m0 --depth 0 >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "--depth 0 exits 2"

# search resolves names within the selection; a full path bypasses it.
amb=$($RW search "$HD.vcd" --condition 'cnt=1' 2>&1); ambcode=$?
[[ "$ambcode" -eq 1 && "$amb" == *"--scope"* ]] \
  && ok || bad "ambiguous condition name points at --scope (exit=$ambcode)"
if $RW search "$HD.vcd" --condition 'cnt=1' --scope u_m0.u_a >/dev/null 2>&1; then
  ok
else
  bad "--scope resolves an ambiguous condition name"
fi
if $RW search "$HD.vcd" --condition 'hier_deep.u_m0.u_a.cnt[3:0]=1' \
     --exclude cnt >/dev/null 2>&1; then
  ok
else
  bad "a full path bypasses --exclude"
fi

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
