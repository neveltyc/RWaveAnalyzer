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


def table_rows(lines):
    """(scope, file:line) from the `-transcript` table.

    Read alongside the `-tcl` form, not instead of it: on a variable vsim
    reports an internal error, marks the result *PARTIAL*, and still prints a
    row that the `-tcl` form returns nothing for. Taking only one form counts
    that as an answer rwave invented.
    """
    out = set()
    for line in lines:
        f = [c.strip() for c in line.split("|")]
        if len(f) == 4 and f[1].startswith("/") and ":" in f[3] and "-" not in f[0]:
            out.add((f[1], f[3]))
    return out


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


# vsim prints a process by its construct, the database stores it by a tag.
# Comparing them means saying so once rather than at each call site.
TAG = {"ALWAYS": "p", "ASSIGN": "a", "INITIAL": "i", "IMPLICIT-WIRE": "w"}


def norm_proc(name):
    # The suffix is not always a bare line number: a process spanning two lines
    # is `#ALWAYS#251,260`, and a generate-replicated one carries a second
    # `#2`. Only the tag is rewritten; whatever follows it is left alone.
    m = re.match(r"#([A-Z-]+)(\(.*\))?#(.+)$", name)
    if not m:
        return name
    tag, paren, rest = m.group(1), m.group(2) or "", m.group(3)
    return "#%s#%s#%s" % (TAG.get(tag, tag), paren, rest) if paren else "#%s#%s" % (TAG.get(tag, tag), rest)


def norm_endpoint(path):
    """`/tb/#ALWAYS#67` -> `/tb/#p#67`, so both sides spell a process the same."""
    scope, _, leaf = path.rpartition("/")
    return scope + "/" + norm_proc(leaf)


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
    # vopt's own temporaries are in the design database but are not signals
    # anyone traces, and neither backend should be judged on them.
    sigs = sorted({s for s in sigs if "dbgTemp" not in s.rsplit("/", 1)[-1]})
    if args.limit:
        sigs = sigs[: args.limit]
    print("signals to check: %d" % len(sigs))
    if not sigs:
        print("FAIL: vsim listed no signals; is the .dbg beside the .wlf?")
        return 1

    cmds = []
    for s in sigs:
        cmds.append("echo [find drivers -possible -tcl {%s}]" % s)
        cmds.append("find drivers -possible -transcript {%s}" % s)
        # `drivers` crosses hierarchy where `find drivers -possible` declines to
        # answer for a port at all, so it is the oracle for exactly the cases
        # the other one leaves blank.
        cmds.append("drivers {%s}" % s)
        cmds.append("readers {%s}" % s)
    truth = vsim_batch(args.vsim, args.wlf, cmds)

    bad, extra = 0, 0
    for i, s in enumerate(sigs):
        want_drv = tcl_rows(truth.get(4 * i, [])) | table_rows(truth.get(4 * i + 1, []))
        want_drv_ep = {norm_endpoint(e) for e in endpoints(truth.get(4 * i + 2, []), "Driver")}
        want_ld = {norm_endpoint(e) for e in endpoints(truth.get(4 * i + 3, []), "Reader")}
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
        # vsim emits a row per source line of a primitive; rwave names the
        # statement once, as the NPI backend does. So the check is that rwave
        # missed nothing: every (scope, file) vsim found must be present, with a
        # line vsim also named.
        want_by_place = {}
        for scope, loc in want_drv:
            f, _, ln = loc.rpartition(":")
            want_by_place.setdefault((scope, f), set()).add(ln)
        got_by_place = {}
        for scope, loc in got_drv:
            f, _, ln = loc.rpartition(":")
            got_by_place.setdefault((scope, f), set()).add(ln)
        missing = [k for k in want_by_place if k not in got_by_place]
        wrong = [
            k for k in want_by_place if k in got_by_place and not (got_by_place[k] & want_by_place[k])
        ]
        if missing or wrong:
            print("\n%s drivers differ\n  vsim : %s\n  rwave: %s" % (s, sorted(want_drv), sorted(got_drv)))
            bad += 1
        got_drv_ep = {to_questa(h["scope"]) + "/" + h["raw_kind"] for h in got["drivers"]}
        if want_drv_ep - got_drv_ep:
            print(
                "\n%s drivers missing (by endpoint)\n  vsim : %s\n  rwave: %s"
                % (s, sorted(want_drv_ep), sorted(got_drv_ep))
            )
            bad += 1
        elif got_by_place.keys() - want_by_place.keys() and not got_drv_ep & want_drv_ep:
            # Extra answers are reported but not failed: reading the database
            # finds statements vsim's own view leaves out, and each needs a look
            # rather than an automatic verdict.
            extra += 1
            print("\n%s: rwave reports %s that vsim does not" % (s, sorted(got_by_place.keys() - want_by_place.keys())))

        got, err = rwave_trace(args.rwave, args.wlf, rw_sig, load=True)
        if got is None:
            if want_ld:
                print("\n%s\n  rwave failed: %s\n  vsim loads: %s" % (s, err, sorted(want_ld)))
                bad += 1
            continue
        got_ld = {to_questa(h["scope"]) + "/" + h["raw_kind"] for h in got["loads"]}
        if want_ld - got_ld:
            print("\n%s loads missing\n  vsim : %s\n  rwave: %s" % (s, sorted(want_ld), sorted(got_ld)))
            bad += 1
        elif got_ld - want_ld:
            # A statement that both writes and reads its own net — the hold
            # path of `if (en) q <= d` — is a load of it. vsim's `readers` omits
            # that; NPI reports it, and verify/npi/README.md pins the behaviour.
            # Matching NPI is the target, so it is not counted against us.
            # `got` is the load result by now; the driver endpoints were taken
            # before it was reassigned.
            if got_ld - want_ld <= got_drv_ep:
                continue
            extra += 1
            print("\n%s: rwave reports loads vsim does not: %s" % (s, sorted(got_ld - want_ld)))

    print(
        "\n== RESULT: %d signal(s) checked, %d missing, %d with extra findings =="
        % (len(sigs), bad, extra)
    )
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
