#!/usr/bin/env bash
# Differential test harness: compare `rwave` against the reference Python tool
# `vcd_analyzer.py` across every command, on the committed VCD fixtures and
# stimulus.
#
# This is the regression net for behavioural parity. It is intentionally
# self-contained and degrades gracefully:
#   * If the reference tool is not found, the script SKIPS (exit 0) with a
#     notice, so it is safe to run in a clone/CI without the reference.
#   * Output is normalized for the file path and size (which legitimately differ
#     between a .vcd and its converted .fst, and between machines).
#   * A small set of KNOWN, documented differences (see README "Known
#     differences") are tolerated: the `list --verbose` identifier field, and
#     the "cannot open waveform file" wording. These are matched by signature
#     and do not count as failures.
#
# Locating the reference (first match wins):
#   $VCD_ANALYZER env var (path to vcd_analyzer.py)
#   ../VCD_ANALYZER/vcd_analyzer.py relative to the repo
#   vcd_analyzer.py on $PATH
#
# Usage:
#   verify/differential.sh           # run all, summary only
#   VERBOSE=1 verify/differential.sh  # print a diff for each failure
#
# Exit code is non-zero if any non-known difference is found.
set -uo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd)"
RWAVE="${RWAVE_BIN:-$REPO/target/release/rwave}"
FIX="$REPO/verify/fixtures"
STIM="$REPO/verify/stimulus"

if [[ ! -x "$RWAVE" ]]; then
  echo "error: $RWAVE not found; run: cargo build --release" >&2
  exit 2
fi

# --- locate the reference -------------------------------------------------
find_reference() {
  if [[ -n "${VCD_ANALYZER:-}" && -f "${VCD_ANALYZER:-}" ]]; then
    echo "$VCD_ANALYZER"; return 0
  fi
  if [[ -f "$REPO/../VCD_ANALYZER/vcd_analyzer.py" ]]; then
    echo "$REPO/../VCD_ANALYZER/vcd_analyzer.py"; return 0
  fi
  local onpath
  onpath="$(command -v vcd_analyzer.py 2>/dev/null || true)"
  if [[ -n "$onpath" ]]; then echo "$onpath"; return 0; fi
  return 1
}

REF="$(find_reference || true)"
if [[ -z "$REF" ]]; then
  echo "SKIP: reference tool vcd_analyzer.py not found."
  echo "      Set \$VCD_ANALYZER, place it at ../VCD_ANALYZER/, or put it on \$PATH."
  echo "      (verify/run.sh covers reference-free smoke + VCD/FST parity.)"
  exit 0
fi
PY=(python3 "$REF")

echo "reference: $REF"
echo "rwave:     $RWAVE"
echo

PASS=0; FAIL=0
FAILED=()

tmpd="$(mktemp -d)"
trap 'rm -rf "$tmpd"' EXIT

# Normalize volatile fields: the absolute/relative file path and the byte size
# (differs vcd vs fst). Everything else must match exactly.
normalize() {
  local f="$1"
  sed -E \
    -e "s#${f}#FILE#g" \
    -e 's/"size_bytes":[0-9]+/"size_bytes":N/g' \
    -e 's/^(Size[[:space:]]*:).*bytes$/\1 N/'
}

# Is a (py,rw) output pair one of the documented, tolerated differences?
# Returns 0 (tolerated) or 1 (real difference).
is_known_diff() {
  local desc="$1" pyf="$2" rwf="$3"
  case "$desc" in
    *list-verbose*)
      # The only expected delta is the identifier field: reference prints the
      # VCD id code (a quoted string, possibly a punctuation char like " or \),
      # rwave prints the backend signal index (a number). Accept iff the diff is
      # confined to that field. JSON forms are compared with a JSON-aware strip
      # (regex breaks when the id code is a quote/backslash); the text form drops
      # the trailing id column.
      if head -c1 "$pyf" | grep -q '{'; then
        python3 - "$pyf" "$rwf" <<'PY'
import json, sys
def strip(path):
    d = json.load(open(path))
    for s in d.get("signals", []):
        s.pop("id", None)
    return d
try:
    sys.exit(0 if strip(sys.argv[1]) == strip(sys.argv[2]) else 1)
except Exception:
    sys.exit(1)
PY
        [[ $? -eq 0 ]] && return 0
        return 1
      fi
      # text form: drop the trailing whitespace-separated id token on signal rows
      local pn rn
      pn="$(sed -E 's/^( +[^ ].*[^ ]) +[^ ]+$/\1/' "$pyf")"
      rn="$(sed -E 's/^( +[^ ].*[^ ]) +[^ ]+$/\1/' "$rwf")"
      [[ "$pn" == "$rn" ]] && return 0
      return 1
      ;;
    err-nofile*)
      # "cannot open VCD file" (ref) vs "cannot open waveform file" (rwave).
      grep -q "cannot open" "$pyf" && grep -q "cannot open waveform file" "$rwf" && return 0
      return 1
      ;;
  esac
  return 1
}

# run_diff DESC FILE ARGS...   (ARGS placed before the file)
run_diff() {
  local desc="$1"; shift
  local file="$1"; shift
  local pyf="$tmpd/py.out" rwf="$tmpd/rw.out"
  "${PY[@]}" "$@" "$file" >"$pyf" 2>&1
  "$RWAVE" "$@" "$file" >"$rwf" 2>&1
  normalize "$file" <"$pyf" >"$tmpd/py.n"
  normalize "$file" <"$rwf" >"$tmpd/rw.n"
  if diff -q "$tmpd/py.n" "$tmpd/rw.n" >/dev/null; then
    PASS=$((PASS+1)); return
  fi
  if is_known_diff "$desc" "$tmpd/py.n" "$tmpd/rw.n"; then
    PASS=$((PASS+1)); return
  fi
  FAIL=$((FAIL+1)); FAILED+=("$desc :: $* :: $(basename "$file")")
  if [[ "${VERBOSE:-0}" == "1" ]]; then
    echo "FAIL: $desc :: $* :: $(basename "$file")"
    diff "$tmpd/py.n" "$tmpd/rw.n" | head -30
    echo "------------------------------------------"
  fi
}

# A dump comparison that ignores intra-timestamp event ordering (a documented
# difference: wellen does not preserve the VCD value-change emission order).
# Values, timestamps, and the event set must still match exactly. Uses Python
# (already required for the reference) to sort events within each timestamp.
run_diff_dump_unordered() {
  local desc="$1"; shift
  local file="$1"; shift
  local pyf="$tmpd/py.out" rwf="$tmpd/rw.out"
  "${PY[@]}" "$@" "$file" >"$pyf" 2>&1
  "$RWAVE" "$@" "$file" >"$rwf" 2>&1
  normalize "$file" <"$pyf" >"$tmpd/py.n"
  normalize "$file" <"$rwf" >"$tmpd/rw.n"

  local is_json=0
  case " $* " in *" --json "*) is_json=1;; esac

  DUMP_IS_JSON="$is_json" python3 - "$tmpd/py.n" "$tmpd/rw.n" <<'PY'
import json, os, sys
pa, pb = sys.argv[1], sys.argv[2]
is_json = os.environ.get("DUMP_IS_JSON") == "1"

def load_json(path):
    with open(path) as fh:
        d = json.load(fh)
    # Sort events within each timestamp by path; keep timestamp order.
    evs = d.get("events", [])
    evs.sort(key=lambda e: (e.get("time_ticks", 0), e.get("path", "")))
    d["events"] = evs
    return d

def load_text(path):
    # Group indented event lines under their "T=" header and sort within group.
    out, block = [], []
    with open(path) as fh:
        for line in fh:
            if line.startswith("  "):
                block.append(line)
            else:
                if block:
                    out.extend(sorted(block)); block = []
                out.append(line)
    if block:
        out.extend(sorted(block))
    return out

try:
    if is_json:
        a = load_json(pa); b = load_json(pb)
        sys.exit(0 if a == b else 1)
    else:
        a = load_text(pa); b = load_text(pb)
        sys.exit(0 if a == b else 1)
except Exception:
    # On any parse error, fall back to exact comparison.
    sys.exit(0 if open(pa).read() == open(pb).read() else 1)
PY
  if [[ $? -eq 0 ]]; then
    PASS=$((PASS+1)); return
  fi
  FAIL=$((FAIL+1)); FAILED+=("$desc(unordered) :: $* :: $(basename "$file")")
  if [[ "${VERBOSE:-0}" == "1" ]]; then
    echo "FAIL: $desc(unordered) :: $* :: $(basename "$file")"
    diff "$tmpd/py.n" "$tmpd/rw.n" | head -30
    echo "------------------------------------------"
  fi
}

echo "== core: info / list / dump / summary on VCD fixtures =="
for fix in basic_trace search_trace handshake_trace bus_range_trace escaped_trace; do
  f="$FIX/$fix.vcd"
  run_diff "info-text"           "$f" info
  run_diff "info-json"           "$f" --json info
  run_diff "info-verbose"        "$f" info --verbose
  run_diff "list-text"           "$f" list
  run_diff "list-json"           "$f" --json list
  run_diff "list-verbose"        "$f" list --verbose
  run_diff "list-verbose-json"   "$f" --json list --verbose
  run_diff "dump-text"           "$f" dump
  run_diff "dump-json"           "$f" --json dump
  run_diff "dump-verbose-json"   "$f" --json dump --verbose
  run_diff "summary-text"        "$f" summary
  run_diff "summary-json"        "$f" --json summary
  run_diff "summary-verbose-json" "$f" --json summary --verbose
done

echo "== search: interval / segment / event modes =="
run_diff "search-int-text"   "$FIX/search_trace.vcd" search --condition "valid=1"
run_diff "search-int-json"   "$FIX/search_trace.vcd" --json search --condition "valid=1"
run_diff "search-int-2cond"  "$FIX/search_trace.vcd" --json search --condition "valid=1,ready=1"
run_diff "search-int-hex"    "$FIX/search_trace.vcd" --json search --condition "data=0x20"
run_diff "search-int-bin"    "$FIX/search_trace.vcd" --json search --condition "data=b00100000"
run_diff "search-int-x"      "$FIX/search_trace.vcd" --json search --condition "valid=x"
run_diff "search-int-ne"     "$FIX/search_trace.vcd" --json search --condition "valid!=0"
run_diff "search-seg-text"   "$FIX/search_trace.vcd" search --condition "valid=1" --show data
run_diff "search-seg-json"   "$FIX/search_trace.vcd" --json search --condition "valid=1" --show data
run_diff "search-seg-multi"  "$FIX/search_trace.vcd" --json search --condition "valid=1" --show "data,ready"
run_diff "search-evt-text"   "$FIX/search_trace.vcd" search --condition "valid=1" --changed data
run_diff "search-evt-json"   "$FIX/search_trace.vcd" --json search --condition "valid=1" --changed data
run_diff "search-evt-tap"    "$FIX/search_trace.vcd" --json search --condition "tap=1" --changed tap
run_diff "search-win"        "$FIX/search_trace.vcd" --json search --condition "valid=1" --begin 10 --end 50
run_diff "search-win2"       "$FIX/search_trace.vcd" --json search --condition "valid=1" --show data --begin 0 --end 100
run_diff "search-hs-int"     "$FIX/handshake_trace.vcd" --json search --condition "valid=1"
run_diff "search-hs-seg"     "$FIX/handshake_trace.vcd" --json search --condition "valid=1" --show "ready"
run_diff "search-limit"      "$FIX/search_trace.vcd" --json --limit 1 search --condition "valid=1" --changed valid
run_diff "search-verbose"    "$FIX/search_trace.vcd" --json search --condition "valid=1" --show data --verbose

echo "== misc: snapshot / compare / windows / filters / errors =="
for fix in basic_trace search_trace handshake_trace bus_range_trace; do
  f="$FIX/$fix.vcd"
  run_diff "snap@20-text"   "$f" snapshot --at 20
  run_diff "snap@20-json"   "$f" --json snapshot --at 20
  run_diff "snap@20ns-json" "$f" --json snapshot --at 20ns
  run_diff "snap@0-json"    "$f" --json snapshot --at 0
  run_diff "snap-verbose"   "$f" --json snapshot --at 20 --verbose
  run_diff "snap-filter"    "$f" --json snapshot --at 20 --filter clk,data
done
for fix in basic_trace search_trace handshake_trace; do
  f="$FIX/$fix.vcd"
  run_diff "cmp-text"    "$f" compare --at 10,30
  run_diff "cmp-json"    "$f" --json compare --at 10,30
  run_diff "cmp-verbose" "$f" --json compare --at 0,30 --verbose
  run_diff "cmp-ns"      "$f" --json compare --at 10ns,30ns
done
run_diff "dump-win"        "$FIX/basic_trace.vcd" --json dump --begin 10 --end 25
run_diff "dump-filter"     "$FIX/basic_trace.vcd" --json dump --filter clk
run_diff "dump-limit"      "$FIX/basic_trace.vcd" --json --limit 3 dump
run_diff "dump-limit-text" "$FIX/basic_trace.vcd" --limit 3 dump
run_diff "summ-win"        "$FIX/search_trace.vcd" --json summary --begin 10 --end 90
run_diff "summ-filter"     "$FIX/basic_trace.vcd" --json summary --filter clk,state
run_diff "err-nofile"      "$FIX/nonexistent.vcd" info
run_diff "err-badtime"     "$FIX/basic_trace.vcd" snapshot --at 10.5
run_diff "err-badrange"    "$FIX/basic_trace.vcd" dump --begin 30 --end 10
run_diff "err-cmp-order"   "$FIX/basic_trace.vcd" compare --at 30,10
run_diff "err-search-nocond" "$FIX/basic_trace.vcd" search --condition "nosuchsig=1"
# time-value edge cases (regression coverage for the parsing fixes)
run_diff "time-too-large"  "$FIX/basic_trace.vcd" snapshot --at 99999999999999999999
run_diff "time-i64max1"    "$FIX/basic_trace.vcd" snapshot --at 9223372036854775808
run_diff "time-underscore" "$FIX/basic_trace.vcd" snapshot --at 1_000
run_diff "time-underscore2" "$FIX/basic_trace.vcd" snapshot --at 1_0_0_0
run_diff "time-bad-underscore" "$FIX/basic_trace.vcd" snapshot --at 1__000

echo "== edge-case designs (value formatting; dump compared order-agnostically) =="
for d in edge_cases wide_bus; do
  f="$STIM/$d.vcd"
  run_diff "owa-info"       "$f" info
  run_diff "owa-list"       "$f" list
  run_diff "owa-summary"    "$f" summary
  run_diff "owa-snapshot"   "$f" --json snapshot --at 30ns
  run_diff "owa-compare"    "$f" --json compare --at 0,90ns
  run_diff_dump_unordered "owa-dump" "$f" dump
  run_diff_dump_unordered "owa-dump-json" "$f" --json dump
done

echo
echo "================= RESULT: PASS=$PASS FAIL=$FAIL ================="
if [[ "$FAIL" -gt 0 ]]; then
  echo "--- failures ---"
  for x in "${FAILED[@]}"; do echo "  $x"; done
fi
[[ "$FAIL" -eq 0 ]]
