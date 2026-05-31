#!/usr/bin/env python3
"""
rwave performance harness against the committed synthetic FST.

The benchmark dataset (`bench/stress.fst`) is ~2 MB after FST's internal LZ4
compression; we ship the **xz-recompressed** `bench/stress.fst.xz` (~160 KB)
in the repo and auto-decompress at run time. This keeps cross-version perf
comparisons fully reproducible: same bytes, same numbers (modulo host noise).

The dataset itself was produced by `bench/gen.py` (deterministic, fixed
LFSR seed). To regenerate, see that file's header.

Output: a compact Markdown table to stdout, plus a JSON dump to
`bench/build/results.json` for downstream consumption (CI release notes).

Requires: GNU time (`gtime` on macOS via `brew install gnu-time`,
`/usr/bin/time` on Linux), `xz` for decompression (preinstalled almost
everywhere).
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
DEFAULT_FST  = HERE / "stress.fst"
DEFAULT_XZ   = HERE / "stress.fst.xz"
DEFAULT_RW   = HERE.parent / "target" / "release" / "rwave"
DEFAULT_OUT  = HERE / "build" / "results.json"


def ensure_fst(fst: Path, xz: Path) -> None:
    """If the .fst is missing but .xz is present, decompress it. Progress
    messages go to stderr so stdout stays clean (CI captures stdout as the
    release-body markdown via `tee`)."""
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
    """Run a command under GNU time. Returns (wall_seconds, max_rss_kb)."""
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

    # Each case carries a short column header (used in the compact markdown
    # table) plus the full argv passed to rwave.
    #
    # Note on the dump window: an unbounded full-file `dump --limit 0` on this
    # dataset peaks at ~7 GB RSS while formatting the full 77M-event JSON
    # output — tight on standard CI runners (ubuntu-latest = 7 GB RAM). The
    # narrower window keeps the bench reproducible while still demonstrating
    # the cost of unbounded output vs. the bounded heap.
    cases = [
        ("info",      ["--json", "info", fst_str]),
        ("list",      ["--json", "list", fst_str]),
        ("summary",   ["--json", "summary", fst_str]),
        ("snapshot",  ["--json", "snapshot", fst_str, "--at", "750us"]),
        ("compare",   ["--json", "compare", fst_str, "--at", "375us,1125us"]),
        ("dump200",   ["--json", "dump", fst_str]),
        ("dump-win",  ["--json", "--limit", "0", "dump", fst_str,
                       "--begin", "0", "--end", "300us"]),
        ("dump-flt",  ["--json", "dump", fst_str, "--filter", "clk_main"]),
        ("search",    ["--json", "search", fst_str, "--condition", "rst_n=1"]),
    ]

    # Collect measurements first
    measurements = []
    for label, argv in cases:
        wall_s, rss_kb = bench(gtime, [str(rw), *argv])
        measurements.append((label, wall_s, rss_kb / 1024))

    # ---- compact 3-row markdown (one column per command) ------------------
    headers = ["metric"] + [m[0] for m in measurements]
    wall_row = ["wall (s)"] + [f"{m[1]:.2f}" for m in measurements]
    rss_row  = ["RSS (MB)"] + [f"{m[2]:.0f}" for m in measurements]

    def md_row(cells, align_right_after=0):
        return "| " + " | ".join(cells) + " |"

    print("# rwave bench results")
    print()
    print(f"- rwave version: `{rwave_version}`")
    print(f"- dataset: `{fst.name}` ({fst_size_mb:.1f} MB)")
    print(f"- host: `{platform.platform()}`")
    print()
    print(md_row(headers))
    # Right-align all data columns (left-align only the "metric" label column)
    print("|" + "---|" + ("---:|" * (len(headers) - 1)))
    print(md_row(wall_row))
    print(md_row(rss_row))
    print()
    print("Legend:")
    print("- `snapshot` @ 750us, `compare` 375us↔1125us")
    print("- `dump200` = default `--limit 200`")
    print("- `dump-win` = `--limit 0 --begin 0 --end 300us` (windowed full output)")
    print("- `dump-flt` = `--filter clk_main` (one signal)")
    print("- `search` condition = `rst_n=1`")

    # ---- JSON dump (full labels preserved for downstream consumers) -------
    full_labels = {
        "info":      "info",
        "list":      "list",
        "summary":   "summary",
        "snapshot":  "snapshot @50%",
        "compare":   "compare 25%↔75%",
        "dump200":   "dump --limit 200",
        "dump-win":  "dump --limit 0, 0..300us",
        "dump-flt":  "dump --filter clk_main",
        "search":    "search rst_n=1",
    }
    results = [
        {
            "command":      full_labels[label],
            "short":        label,
            "wall_s":       wall_s,
            "rss_mb":       round(rss_mb, 1),
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
