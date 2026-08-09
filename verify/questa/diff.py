#!/usr/bin/env python3
"""Differential check: rwave's WLF trace against QuestaSim's own answers.

`trace` on a `.wlf` reads Questa's debug database directly, and that database
is undocumented — a misread column produces a wrong answer, not a crash. vsim
can answer the same questions from the same file, so every answer is compared
against it rather than trusted.

Needs a machine with QuestaSim. Build the design first (see README.md), then:

    python3 verify/questa/diff.py --wlf run.wlf --rwave ./rwave

Compares, per signal:
  drivers  rwave's (file:line) set   vs  `find drivers -possible -tcl`
  loads    rwave's endpoint set      vs  `readers`

Exit 0 when every signal agrees, 1 otherwise, with both sides printed for each
disagreement.
"""

import argparse
import json
import os
import re
import subprocess
import sys

MARK = "@@RW@@"


def vsim_batch(vsim, wlf, commands):
    """Run one vsim session over `commands`, returning the output per command.

    One session, not one per command: startup is ~1.5 s and a real design has
    thousands of signals. Each command is bracketed by an echo marker so its
    output can be cut back out, which also makes a command that printed nothing
    distinguishable from one still in flight.
    """
    script = []
    for i, c in enumerate(commands):
        script.append("echo {%s %d}" % (MARK, i))
        script.append(c)
    script.append("echo {%s end}" % MARK)
    script.append("quit -f")
    out = subprocess.run(
        [vsim, "-c", "-nolog", "-view", os.path.basename(wlf)],
        input="\n".join(script) + "\n",
        capture_output=True,
        text=True,
        cwd=os.path.dirname(os.path.abspath(wlf)) or ".",
    ).stdout

    chunks, cur, idx = {}, [], None
    for line in out.splitlines():
        if line.startswith("VSIM ") and "> " in line[:24]:
            continue  # vsim echoing the command it just read
        body = line[2:] if line.startswith("# ") else line
        m = re.search(re.escape(MARK) + r" (\d+|end)", body)
        if m:
            if idx is not None:
                chunks[idx] = cur
            cur = []
            idx = None if m.group(1) == "end" else int(m.group(1))
            continue
        if idx is not None:
            cur.append(body)
    return chunks


def tcl_rows(lines):
    """(scope, file:line) from `find drivers -possible -tcl` output."""
    out = set()
    for line in lines:
        for row in re.findall(r"\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}", line):
            f = re.findall(r"\{[^{}]*\}|\S+", row)
            f = [x.strip("{}") for x in f]
            if len(f) == 4 and f[1].startswith("/"):
                out.add((f[1], f[3]))
    return out


def endpoints(lines, tag):
    """Process paths from `drivers` / `readers` output."""
    out = set()
    for line in lines:
        i = line.find(": %s " % tag)
        if i >= 0:
            out.add(line[i + len(tag) + 3 :].strip())
    return out


def to_questa(path):
    return "/" + path.replace(".", "/")


def rwave_trace(rwave, wlf, sig, load):
    argv = [rwave, "trace", wlf, sig, "--json", "--limit", "0"]
    if load:
        argv.append("--load")
    r = subprocess.run(argv, capture_output=True, text=True)
    if r.returncode != 0:
        return None, r.stderr.strip()
    return json.loads(r.stdout), None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wlf", required=True)
    ap.add_argument("--rwave", default="rwave")
    ap.add_argument("--vsim", default="vsim")
    ap.add_argument("--limit", type=int, default=0, help="check at most N signals")
    args = ap.parse_args()

    names = vsim_batch(args.vsim, args.wlf, ["echo [find signals -r /*]"])
    sigs = []
    for line in names.get(0, []):
        sigs += [s for s in line.split() if s.startswith("/")]
    sigs = sorted(set(sigs))
    if args.limit:
        sigs = sigs[: args.limit]
    print("signals to check: %d" % len(sigs))
    if not sigs:
        print("FAIL: vsim listed no signals; is the .dbg beside the .wlf?")
        return 1

    cmds = []
    for s in sigs:
        cmds.append("echo [find drivers -possible -tcl {%s}]" % s)
        cmds.append("readers {%s}" % s)
    truth = vsim_batch(args.vsim, args.wlf, cmds)

    bad = 0
    for i, s in enumerate(sigs):
        want_drv = tcl_rows(truth.get(2 * i, []))
        want_ld = endpoints(truth.get(2 * i + 1, []), "Reader")
        rw_sig = s.lstrip("/").replace("/", ".")

        got, err = rwave_trace(args.rwave, args.wlf, rw_sig, load=False)
        if got is None:
            # A signal vsim lists but rwave will not trace is only acceptable
            # when there was nothing to find in the first place.
            if want_drv:
                print("\n%s\n  rwave failed: %s\n  vsim drivers: %s" % (s, err, sorted(want_drv)))
                bad += 1
            continue
        got_drv = {
            (to_questa(h["scope"]), "%s:%s" % (h["file"], h["line"]))
            for h in got["drivers"]
            if h["file"] and h["line"]
        }
        if got_drv != want_drv:
            print("\n%s drivers differ\n  vsim : %s\n  rwave: %s" % (s, sorted(want_drv), sorted(got_drv)))
            bad += 1

        got, err = rwave_trace(args.rwave, args.wlf, rw_sig, load=True)
        if got is None:
            if want_ld:
                print("\n%s\n  rwave failed: %s\n  vsim loads: %s" % (s, err, sorted(want_ld)))
                bad += 1
            continue
        got_ld = {to_questa(h["scope"]) + "/" + h["raw_kind"] for h in got["loads"]}
        if got_ld != want_ld:
            print("\n%s loads differ\n  vsim : %s\n  rwave: %s" % (s, sorted(want_ld), sorted(got_ld)))
            bad += 1

    print("\n== RESULT: %d signal(s) checked, %d disagreement(s) ==" % (len(sigs), bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
