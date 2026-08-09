# Differential checks for `trace` on a WLF

`trace` on a `.wlf` reads Questa's post-simulation debug database (`.dbg`)
directly. That database is undocumented, so a misread column gives a wrong
answer rather than an error. QuestaSim can answer the same questions from the
same file, so `diff.py` asks both and compares — this is the check that the
reader is right, and it is worth more than any assertion written from the
outside. It has earned that: every decoding rule in the reader that was wrong
was wrong plausibly, and each was found here rather than by reading the schema.

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

Expected: `RESULT: N signal(s) checked, 0 missing, M answered beyond vsim`.
A missing answer is a failure and prints both sides. `--limit N` checks a prefix
of the signal list while iterating.

## Answers vsim does not give

`M` is not a defect count. vsim declines to answer several questions it is
asked, and rwave, reading the database rather than a rendered view, answers
them. Three classes have been checked against the RTL:

| class | why vsim is silent | checked against |
|:--|:--|:--|
| a variable rather than a net — picorv32's `set_mem_do_rdata` | `find drivers -possible` covers nets; on a variable it reports an internal error and marks the result *PARTIAL* | `picorv32.v:1199`, a `reg` set at 1407 and 1898 and read at 1958 |
| a clocked memory as a load of its clock — `cpuregs`, `_ram` | `readers` enumerates statements, not declared objects | `picorv32.v:203`, `reg [31:0] cpuregs [0:regfile_size-1]` |
| a port or interface member driven from an enclosing scope | `find drivers -possible` does not follow a port, which is the whole point of the feature | `dut.sv:102`, the `initial` block that drives `b.data`; `tinyriscv.v:361`, `.raddr_o(clint_raddr_o)` |

The count is printed rather than hidden: a fourth class would be a new claim,
and it should be looked at rather than assumed to belong to one of these.

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
