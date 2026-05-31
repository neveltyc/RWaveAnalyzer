#!/usr/bin/env bash
# Regenerate the test stimulus in verify/stimulus/ from the Verilog sources in
# verify/stimulus_src/. Each testbench is compiled to a VCD with Icarus Verilog,
# then converted to FST with vcd2fst, giving a matched (.vcd, .fst) pair per
# design so the analyzer can be exercised on both formats.
#
# **Metadata sanitization.** Icarus stamps each VCD with `$date` (the wall-clock
# time of generation) and `$version` (the simulator banner). Those values leak
# host state and are non-reproducible across machines and runs. Before the FST
# is materialized, the VCD `$date` and `$version` block contents are replaced
# with fixed placeholders, so the committed stimulus is identical regardless of
# who (or what CI runner) regenerated it. The FST inherits the sanitized
# header from the post-processed VCD.
#
# Prerequisites: iverilog, vvp, vcd2fst (Debian/Ubuntu: iverilog, gtkwave).
set -euo pipefail

cd "$(dirname "$0")/.."
SRC=verify/stimulus_src
OUT=verify/stimulus
mkdir -p "$OUT"

tbs=(counter_fsm xz_tristate hier_deep real_event handshake_proto)

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Filter: replace the body of `$date` and `$version` blocks with one fixed
# placeholder line each. Everything else passes through unchanged.
sanitize_vcd() {
  awk '
    /^\$date$/    { print; print "    1970-01-01 00:00:00 UTC"; skip = 1; next }
    /^\$version$/ { print; print "    rwave-stimulus";            skip = 1; next }
    skip && /^\$end$/ { print; skip = 0; next }
    skip { next }
    { print }
  ' "$1"
}

for tb in "${tbs[@]}"; do
  echo ">> $tb"
  iverilog -o "$tmp/$tb.vvp" "$SRC/$tb.v"
  # vvp writes the $dumpfile (named in the .v) into its CWD.
  ( cd "$tmp" && vvp "$tb.vvp" >/dev/null )

  sanitize_vcd "$tmp/$tb.vcd" > "$OUT/$tb.vcd"
  vcd2fst -v "$OUT/$tb.vcd" -f "$OUT/$tb.fst" >/dev/null 2>&1

  echo "   $(wc -c < "$OUT/$tb.vcd") B vcd, $(wc -c < "$OUT/$tb.fst") B fst"
done

echo ">> Stimulus regenerated in $OUT/ (metadata-normalized)"
