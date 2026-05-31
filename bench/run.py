#!/usr/bin/env python3
"""
rwave performance harness against a real Verilator/VeeRwolf+Zephyr trace.

The benchmark dataset (`bench/stress.fst`) is a real Verilator-generated
trace of [VeeRwolf](https://github.com/chipsalliance/Cores-VeeR-EL2) — a
RISC-V EL2 core — running [Zephyr RTOS](https://github.com/zephyrproject-rtos),
captured by [@neveltyc](https://github.com/neveltyc). The raw FST is
~63 MB; we ship `stress.fst.xz` (~8 MB) and auto-decompress at run time.

The bench mirrors a **realistic agent debug workflow**:

  1. `search-irq`   — find all interrupt entries; capture PC + cause + insn +
                      tval in one shot (replaces N snapshot calls).
  2. `dump-handler` — trace PC during one handler (a narrow time window).
  3. `snap-mret`    — verify the return point's state.
  4. `search-mret`  — find every `MRET` (insn == 0x30200073) in the trace.
  5. `dump-pc-full` — full-time-range dump of one signal (PC).
  6. `sum-dec`      — heavyweight: summary over the decode block (~5 GB RSS).

Plus the basic `info` / `list` / `list-rv` header queries.

Output: a compact 3-row Markdown table (metric × command) to stdout +
JSON dump to `bench/build/results.json` (consumed by the CI release-notes
step).

Note on the *un*filtered cases: queries with no `--filter` on this trace
peak at 7–8 GB RSS and run 50+ s — outside the 7 GB standard CI runner's
budget. They're deliberately *not* benched; the bench instead reflects
the right way to drive rwave on a real waveform.
"""
from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


HERE = Path(__file__).resolve().parent
DEFAULT_FST = HERE / "stress.fst"
DEFAULT_XZ  = HERE / "stress.fst.xz"
DEFAULT_RW  = HERE.parent / "target" / "release" / "rwave"
DEFAULT_OUT = HERE / "build" / "results.json"

# Signal paths we'll refer to. These are real VeeRwolf RV-tracer probes
# exported via the RV trace interface.
PC_SIG     = "trace_rv_i_address_ip[63:0]"
ECAUSE_SIG = "trace_rv_i_ecause_ip[4:0]"
INSN_SIG   = "trace_rv_i_insn_ip[63:0]"
TVAL_SIG   = "trace_rv_i_tval_ip[31:0]"
EXC_SIG    = "trace_rv_i_exception_ip[2:0]"
RV_FILTER  = "trace_rv_i"        # all 24 trace_rv paths
DEC_FILTER = "veer.dec"          # the decoder block — heavyweight summary
MRET_INSN  = "0x30200073"        # MRET encoding


def ensure_fst(fst: Path, xz: Path) -> None:
    """Decompress .xz when only .fst is missing. Progress to stderr."""
    if fst.exists():
        return
    if not xz.exists():
        sys.exit(f"error: neither {fst} nor {xz} exists")
    print(f"  decompressing {xz.name} -> {fst.name} ...", file=sys.stderr)
    subprocess.run(["xz", "-dk", str(xz)], check=True)


def find_gtime() -> str:
    for cand in ("gtime", "/usr/bin/time"):
        if shutil.which(cand):
            r = subprocess.run([cand, "-f", "%e %M", "true"],
                               capture_output=True, text=True)
            if r.returncode == 0:
                return cand
    sys.exit("error: GNU time not found (install: 'brew install gnu-time' "
             "or distro 'time' package)")


def bench(gtime: str, args: list[str]) -> tuple[float, int]:
    with tempfile.NamedTemporaryFile("r+", delete=False) as tmp:
        subprocess.run([gtime, "-f", "%e %M", *args],
                       stdout=subprocess.DEVNULL, stderr=tmp)
        tmp.seek(0)
        last = tmp.read().strip().splitlines()[-1]
    os.unlink(tmp.name)
    wall_s, rss_kb = last.split()
    return float(wall_s), int(rss_kb)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--fst",    default=str(DEFAULT_FST))
    p.add_argument("--xz",     default=str(DEFAULT_XZ))
    p.add_argument("--rwave",  default=str(DEFAULT_RW))
    p.add_argument("--output", default=str(DEFAULT_OUT))
    args = p.parse_args()

    fst = Path(args.fst).resolve()
    xz  = Path(args.xz).resolve()
    rw  = Path(args.rwave).resolve()

    ensure_fst(fst, xz)
    if not rw.exists():
        sys.exit(f"error: rwave binary not found: {rw}\n"
                 f"Run: cargo build --release")

    gtime = find_gtime()
    fst_str = str(fst)

    ver_out = subprocess.run([str(rw), "--version"], capture_output=True, text=True)
    rwave_version = ver_out.stdout.strip().split()[-1] if ver_out.returncode == 0 else "?"
    fst_size_mb = fst.stat().st_size / 1_000_000

    cases = [
        ("info",         ["--json", "info", fst_str]),
        ("list",         ["--json", "list", fst_str]),
        ("list-rv",      ["--json", "list", fst_str, "--filter", RV_FILTER]),
        # The real-debug headline: find every interrupt entry + capture context.
        # One call replaces N snapshot calls.
        ("search-irq",   ["--json", "search", fst_str,
                          "--condition", f"{EXC_SIG}!=0",
                          "--show", f"{PC_SIG},{ECAUSE_SIG},{INSN_SIG},{TVAL_SIG}"]),
        # Find every MRET instruction in the file.
        ("search-mret",  ["--json", "search", fst_str,
                          "--condition", f"{INSN_SIG}={MRET_INSN}"]),
        # Trace PC during one handler (narrow time window).
        ("dump-handler", ["--json", "--limit", "0", "dump", fst_str,
                          "--begin", "260.8ns", "--end", "272ns",
                          "--filter", PC_SIG]),
        # Snapshot at the handler's mret-return point.
        ("snap-mret",    ["--json", "snapshot", fst_str, "--at", "271.5ns",
                          "--filter", f"{PC_SIG},{INSN_SIG},{ECAUSE_SIG}"]),
        # Whole-trace PC dump — one signal, 20us, fully unbounded.
        ("dump-pc-full", ["--json", "--limit", "0", "dump", fst_str,
                          "--filter", PC_SIG]),
        # Heavyweight: summary over the decode block (~5 GB RSS, ~10 s).
        ("sum-dec",      ["--json", "summary", fst_str, "--filter", DEC_FILTER]),
    ]

    measurements = []
    for label, argv in cases:
        wall_s, rss_kb = bench(gtime, [str(rw), *argv])
        measurements.append((label, wall_s, rss_kb / 1024))

    # ---- compact 3-row Markdown -----------------------------------------
    headers = ["metric"] + [m[0] for m in measurements]
    wall_row = ["wall (s)"] + [f"{m[1]:.2f}" for m in measurements]
    rss_row  = ["RSS (MB)"] + [f"{m[2]:.0f}" for m in measurements]

    def md_row(cells):
        return "| " + " | ".join(cells) + " |"

    print("# rwave bench results")
    print()
    print(f"- rwave version: `{rwave_version}`")
    print(f"- dataset: `{fst.name}` ({fst_size_mb:.1f} MB) — VeeRwolf RISC-V "
          f"EL2 core + Zephyr RTOS boot, 10 k signals, 20 us sim, "
          f"Verilator-generated FST")
    print(f"- host: `{platform.platform()}`")
    print()
    print(md_row(headers))
    print("|" + "---|" + ("---:|" * (len(headers) - 1)))
    print(md_row(wall_row))
    print(md_row(rss_row))
    print()
    print("Legend (a realistic RV interrupt-debug workflow + one heavyweight):")
    print(f"- `search-irq`    = find every interrupt entry, return PC + cause + insn + tval in one call")
    print(f"- `search-mret`   = find every `MRET` instruction ({MRET_INSN})")
    print(f"- `dump-handler`  = trace PC over one handler's window (260.8 ns .. 272 ns)")
    print(f"- `snap-mret`     = state at the handler's return point (271.5 ns)")
    print(f"- `dump-pc-full`  = full unbounded PC dump (one signal, 20 us)")
    print(f"- `sum-dec`       = `--filter veer.dec` — the heaviest scope summary that still fits a 7 GB CI runner")
    print()
    print("Unfiltered whole-file commands on this trace consume 7–8 GB RSS")
    print("and 50+ s; they're omitted from CI by design.")

    # ---- JSON dump -------------------------------------------------------
    full_labels = {
        "info":         "info",
        "list":         "list",
        "list-rv":      f"list --filter {RV_FILTER}",
        "search-irq":   f"search --condition {EXC_SIG}!=0 --show <4 fields>",
        "search-mret":  f"search --condition {INSN_SIG}={MRET_INSN}",
        "dump-handler": f"dump --limit 0 --begin 260.8ns --end 272ns --filter {PC_SIG}",
        "snap-mret":    f"snapshot --at 271.5ns --filter <3 fields>",
        "dump-pc-full": f"dump --limit 0 --filter {PC_SIG}",
        "sum-dec":      f"summary --filter {DEC_FILTER}",
    }
    results = [
        {
            "command": full_labels[label],
            "short":   label,
            "wall_s":  wall_s,
            "rss_mb":  round(rss_mb, 1),
        }
        for (label, wall_s, rss_mb) in measurements
    ]
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps({
        "rwave_version": rwave_version,
        "fst_name":      fst.name,
        "fst_size_mb":   round(fst_size_mb, 1),
        "platform":      platform.platform(),
        "results":       results,
    }, indent=2))

    print()
    print(f"_JSON dump: {out_path}_")
    return 0


if __name__ == "__main__":
    sys.exit(main())
