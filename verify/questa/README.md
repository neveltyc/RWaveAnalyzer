# Differential checks for `trace` on a WLF

`trace` on a `.wlf` reads Questa's post-simulation debug database (`.dbg`)
directly. That database is undocumented, so a misread column gives a wrong
answer rather than an error. QuestaSim can answer the same questions from the
same file, so `diff.py` asks both and compares — this is the check that the
reader is right, and it is worth more than any assertion written from the
outside.

Needs a machine with QuestaSim and `python3`. `verify/run.sh` cannot cover any
of it.

`dut.sv` carries the shapes that are easy to get wrong: a continuous assign, an
`always_ff` with async reset, an `always_comb` case, an interface with a
modport, a part-select load, two drivers on one net, a three-level hierarchy,
and a counter whose single statement both writes and reads its own net. It is
the same fixture as [`verify/npi/dut.sv`](../npi/dut.sv), so the two backends
can be asked the same questions about the same RTL.

## Build

```bash
export PATH="$PATH:<questa>/linux_x86_64"
vlib work
vlog -sv dut.sv
vopt +acc tb -o tb_opt -debugdb
vsim -c -postsimdataflow -debugdb=run.dbg -wlf run.wlf tb_opt \
     -do 'add log -r /*; run -all; quit -f'
```

`+acc` and `-debugdb` are both required, and `-postsimdataflow` is what makes
the dataset usable afterwards. The `.dbg` must keep the `.wlf`'s basename.

## Run

```bash
python3 verify/questa/diff.py --wlf run.wlf --rwave ./rwave
```

Expected: `RESULT: N signal(s) checked, 0 disagreement(s)`. Every disagreement
prints both sides. `--limit N` checks a prefix of the signal list while
iterating.

Run it on more than the fixture — a wrong column reading often only shows up on
real RTL:

| design | why |
|:--|:--|
| `dut.sv` | the shaped cases above |
| picorv32 | real RTL, wide buses, generate blocks |
| a full SoC | hierarchy depth and port crossing |

## Cross-backend parity

The same `dut.sv` builds under VCS with `+define+FSDB`, which produces
`tb.fsdb`. Running `rwave trace` against the FSDB and against the WLF and
comparing the two answers is the direct test of "the WLF backend agrees with
the NPI one" — a stronger statement than either backend matching its own vendor
tool.
