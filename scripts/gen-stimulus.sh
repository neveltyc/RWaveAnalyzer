#!/usr/bin/env bash
# Regenerate the test stimulus in verify/stimulus/ from the Verilog sources in
# verify/stimulus_src/. Each testbench is compiled to a VCD with Icarus Verilog,
# then converted to FST with vcd2fst, giving a matched (.vcd, .fst) pair per
# design so the analyzer can be exercised on both formats.
#
# Prerequisites: iverilog, vvp, vcd2fst (Debian/Ubuntu: iverilog, gtkwave).
set -euo pipefail

cd "$(dirname "$0")/.."
SRC=verify/stimulus_src
OUT=verify/stimulus
mkdir -p "$OUT"

tbs=(counter_fsm xz_tristate hier_deep real_event handshake_proto edge_cases wide_bus)

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

for tb in "${tbs[@]}"; do
  echo ">> $tb"
  iverilog -o "$tmp/$tb.vvp" "$SRC/$tb.v"
  # vvp writes the $dumpfile (named in the .v) into its CWD.
  ( cd "$tmp" && vvp "$tb.vvp" >/dev/null )
  mv "$tmp/$tb.vcd" "$OUT/$tb.vcd"
  vcd2fst -v "$OUT/$tb.vcd" -f "$OUT/$tb.fst" >/dev/null 2>&1
  echo "   $(wc -c < "$OUT/$tb.vcd") B vcd, $(wc -c < "$OUT/$tb.fst") B fst"
done

echo ">> Stimulus regenerated in $OUT/"
