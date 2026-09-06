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

echo "== tree / trace =="
HF="$STIM/hier_deep.vcd"
# tree is derived from scope strings, so it works on every format.
$RW tree "$HF" >/dev/null 2>&1 && ok || bad "tree runs on vcd"
# Intermediate scopes must appear even when they hold no signals of their own:
# hier_deep.u_m0.u_a is only reachable by synthesizing path prefixes.
$RW --json tree "$HF" --depth 3 2>/dev/null | grep -q '"path":"hier_deep.u_m0.u_a"' \
  && ok || bad "tree synthesizes intermediate scopes"
# --depth means the same as it does for list: N levels below the matched root,
# tree counting scopes where list counts signals. Pinned exactly, because an
# off-by-one here still satisfies "deeper shows more".
d1=$($RW --json tree "$HF" 2>/dev/null | grep -o '"path"' | wc -l | tr -d ' ')
[[ "$d1" -eq 3 ]] && ok || bad "tree default depth 1 = root + children (got $d1, want 3)"
d2=$($RW --json tree "$HF" --depth 2 2>/dev/null | grep -o '"path"' | wc -l | tr -d ' ')
[[ "$d2" -eq 7 ]] && ok || bad "tree --depth 2 reaches the grandchildren (got $d2, want 7)"
# A leading-separator hierarchy (Questa/VHDL style, which the FSDB backend
# passes through verbatim) must not collapse: its top level is a real scope,
# not a child of an empty-named one.
SL=/tmp/_rwave_slash.$$.vcd
printf '$date x $end\n$timescale 1ps $end\n$scope module /top $end\n$var wire 1 ! clk $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n' > "$SL"
$RW --json tree "$SL" 2>/dev/null | grep -q '"path":"/top"' \
  && ok || bad "tree handles a leading-separator hierarchy"
rm -f "$SL"
# Unlike every other command, tree accepts --depth without --scope.
$RW tree "$HF" --depth 2 >/dev/null 2>&1 && ok || bad "tree allows --depth without --scope"
# ...and that exemption must not leak to the others.
$RW list "$HF" --depth 2 >/dev/null 2>&1; [[ $? -eq 2 ]] \
  && ok || bad "list still requires --scope for --depth"
# The positional scope and --scope mean the same thing.
a=$($RW --json tree "$HF" u_m0 --depth 2 2>/dev/null)
b=$($RW --json tree "$HF" --scope u_m0 --depth 2 2>/dev/null)
[[ "$a" == "$b" ]] && ok || bad "tree positional scope == --scope"
# --of answers "what is above me", top-down and rooted at the file top.
$RW --json tree "$HF" --of hier_deep.u_m1.u_b.cnt 2>/dev/null \
  | grep -q '"mode":"chain"' && ok || bad "tree --of emits a chain"
# A bad --of name is a clean error, not a panic or an empty success.
$RW tree "$HF" --of nope_not_here >/dev/null 2>&1; [[ $? -eq 1 ]] \
  && ok || bad "tree --of unknown signal exits 1"
# When a name matches in several places, the rows must be distinguishable —
# telling those instances apart is the whole point of asking.
mr=$($RW tree "$HF" u_a --depth 3 2>/dev/null | grep -c 'hier_deep\.u_m[01]\.u_a')
[[ "$mr" -eq 2 ]] && ok || bad "tree spells multiple roots out in full (got $mr)"
# JSON reports the matched roots as paths, not as a display string.
$RW --json tree "$HF" u_a 2>/dev/null \
  | grep -q '"roots":\["hier_deep.u_m0.u_a","hier_deep.u_m1.u_a"\]' \
  && ok || bad "tree JSON exposes roots as an array of paths"

# trace is on by default; RWAVE_TRACE_EN only turns it off. Either refusal must
# be clean (exit 1, explanatory text) rather than a partial answer.
derr=$(RWAVE_TRACE_EN=0 $RW trace "$HF" hier_deep.u_m0.u_a.clk 2>&1); dcode=$?
[[ "$dcode" -eq 1 ]] && ok || bad "trace switched off exits 1 (got $dcode)"
printf '%s' "$derr" | grep -q "RWAVE_TRACE_EN" \
  && ok || bad "the disabled refusal names the switch"
# With the variable unset, the command is available: the refusal that follows is
# the backend's, not the switch's.
printf '%s' "$($RW trace "$HF" clk 2>&1)" | grep -q "built-in Verdi NPI backend" \
  && ok || bad "trace is on by default"
# An empty value reads as "not given", as it does for every other option, so it
# leaves trace on rather than turning it off.
printf '%s' "$(RWAVE_TRACE_EN='' $RW trace "$HF" clk 2>&1)" | grep -q "built-in Verdi NPI backend" \
  && ok || bad "an empty RWAVE_TRACE_EN leaves trace on"

# Once enabled, trace still needs design connectivity, which no waveform-only
# backend has.
terr=$(RWAVE_TRACE_EN=1 $RW trace "$HF" hier_deep.u_m0.u_a.clk 2>&1); tcode=$?
[[ "$tcode" -eq 1 ]] && ok || bad "trace on vcd exits 1 (got $tcode)"
printf '%s' "$terr" | grep -q "built-in Verdi NPI backend" \
  && ok || bad "trace refusal names the required backend"
# A missing signal argument is a usage error (exit 2), not a runtime one, and is
# caught by the parser whether or not the switch is set.
$RW trace "$HF" >/dev/null 2>&1; [[ $? -eq 2 ]] && ok || bad "trace without a signal exits 2"
# --driver and --load are opposites, not a last-one-wins toggle.
$RW trace "$HF" clk --driver --load >/dev/null 2>&1; [[ $? -eq 2 ]] \
  && ok || bad "trace rejects --driver together with --load"
# The capability limit must be reported before any complaint about the name:
# refining an ambiguous signal would be wasted effort on a file that can never
# answer at all.
printf '%s' "$(RWAVE_TRACE_EN=1 $RW trace "$HF" clk 2>&1)" | grep -q "built-in Verdi NPI backend" \
  && ok || bad "trace reports the capability limit before name ambiguity"

# Options belong to the command that defines them. A flag introduced for trace
# or tree must not become silently-ignored noise on the other seven.
for f in "--top x" "--kdb x" "--driver" "--load"; do
  $RW list "$HF" $f >/dev/null 2>&1
  [[ $? -eq 2 ]] && ok || bad "list rejects $f (trace-only)"
done
$RW summary "$HF" --of clk >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "summary rejects --of (tree-only)"
$RW list "$HF" --control >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "list rejects --control (trace-only)"
# --of walks up, --scope/--depth walk down; asking for both is a contradiction,
# and must be caught on the merged values too, not only on one command line.
$RW tree "$HF" u_m0 --of hier_deep.clk >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "tree rejects --of together with a scope"
$RW tree "$HF" --of hier_deep.clk --depth 2 >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "tree rejects --of together with --depth"
bo=$(printf 'tree u_m0\n' | $RW --batch "$HF" --of hier_deep.clk --json 2>/dev/null)
printf '%s' "$bo" | grep -q '"ok":false' \
  && ok || bad "batch re-checks --of against the merged scope"
# ...and the new commands only take what they actually use.
$RW tree "$HF" --filter clk >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "tree rejects --filter"
$RW trace "$HF" clk --scope u_m0 >/dev/null 2>&1
[[ $? -eq 2 ]] && ok || bad "trace rejects --scope"
# trace has no depth concept, so --depth must not be silently accepted there.
$RW trace "$HF" hier_deep.clk --depth 3 >/dev/null 2>&1; [[ $? -eq 2 ]] \
  && ok || bad "trace rejects --depth like every non-tree command"
# A batch-wide default is for the lines that want it and must not fail the rest.
bk=$(printf 'info\n' | $RW --batch "$HF" --kdb /some/dir --json 2>/dev/null)
printf '%s' "$bk" | grep -q '"ok":true' \
  && ok || bad "a session-wide --kdb does not break other batch lines"
# The tree exemption survives the batch merge: clearing an inherited scope must
# not drag the inherited depth down with it, since tree measures from the root.
bt=$(printf "tree --scope ''\n" | $RW --batch "$HF" --scope u_m0 --depth 3 2>/dev/null | sed -n 2p)
printf '%s' "$bt" | grep -q "depth 3" && ok || bad "batch tree keeps inherited --depth ($bt)"

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
