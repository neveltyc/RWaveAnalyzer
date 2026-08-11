# Differential checks for `trace` on a WLF

`trace` on a `.wlf` reads Questa's post-simulation debug database (`.dbg`)
directly. That database is undocumented, so a misread column gives a wrong
answer rather than an error. QuestaSim can answer the same questions from the
same file, so `diff.py` asks both and compares. That is the check that the
reader is right, and it is worth more than any assertion written from the
outside: a decoding rule that is wrong is wrong plausibly, and shows up here
rather than in a schema nobody has.

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

### The construct fixture

`constructs.sv` is the other half of the check, and the more useful one.
`dut.sv` and the open-source designs are RTL somebody wrote for their own
reasons; whether they happen to contain the construct that breaks the reader is
luck, and every fault found on them was found by accident. This file contains
one instance of each *kind* of connectivity SystemVerilog can express, taken
from slang's syntax node table rather than from memory: port forms (ANSI,
non-ANSI, concatenated, wildcard, empty), instantiation (arrays, gate
primitives, `bind`), generate (`for`, `if`, `case`, unnamed, nested),
interfaces (modport, whole, arrays), packed structs and unions and their
members, every assignment form, `wand`/`wor`/`tri` and implicit nets, memories,
functions and tasks, and a hierarchical reference across instances.

`constructs_vhdl.vhd` is the same idea in the other language — an entity with
ports of each direction, a clocked process, a concurrent and a conditional
assignment, a component instantiation and a for-generate — instantiated from
the SystemVerilog top, because `rw_du_tbl` carries a `lang` column and a
design unit that is not Verilog had never been asked about.

```bash
vlib work && vcom constructs_vhdl.vhd && vlog -sv -mfcu constructs.sv
vopt +acc tb -o tb_opt -debugdb
vsim -c -postsimdataflow -debugdb=cx.dbg -wlf cx.wlf tb_opt \
     -do 'add log -r /*; run -all; quit -f'
python3 verify/questa/diff.py --wlf cx.wlf --rwave ./rwave
```

239 signals and a few seconds a run, against forty minutes for a shard of a
real SoC. Each construct sits in a scope named after it, so a disagreement
names the construct rather than a line number.

## Run

```bash
python3 verify/questa/diff.py --wlf run.wlf --rwave ./rwave
```

Expected: `RESULT: N signal(s) checked, 0 missing, M answered beyond vsim, K
object endpoints not compared`. A missing answer is a failure and prints both
sides, and so does every difference behind `M` — a count on its own says how
many to look at without saying which.

`K` counts endpoints vsim names that are not statements. Both tools spell a
statement with a `#tag#`; a name without one is a declared object, and the two
do not enumerate the same ones. On a design full of behavioural cells `readers`
lists every pin on the net, which rwave reports as a port hop and then drops
once it has the statement. Comparing those measures the difference in what each
enumerates, so both sides are filtered and what was removed is printed.

`--shard I/N` checks signals `I, I+N, I+2N…`. One shard samples the whole
hierarchy where `--limit` takes an alphabetical prefix, which on a large design
covers only the first scope: veerwolf's first 60 signals disagree nowhere, and
1852 spread across it disagreed in 160 places. The N shards together cover every
signal.

## What this reader means by a driver

QuestaSim is the oracle for whether an answer is right, which leaves open what
the question is. [slang](https://github.com/MikePopoloski/slang) states one, as
a front end that computes drivers from the source rather than reading somebody
else's elaboration, and comparing the two models says where this one stops.

slang's `ValueDriver` carries four things: a **kind** (procedural or
continuous), a **source** (`initial`, `final`, `always`, `always_comb`,
`always_latch`, `always_ff`, subroutine), **flags** (input port, output port,
clocking variable, initializer, via an indirect port such as a modport), and a
**path** — the longest statically known prefix of what is assigned, with the
**bit range** it covers. Two drivers conflict when their ranges overlap.

A `Hop` here carries the kind, the statement and where it is, the scope, and
whether reaching it crossed a port. It does **not** carry a bit range, and it
folds `initial` in with the other procedural sources. That has one consequence
worth stating plainly rather than discovering: **a question about part of an
object is refused, not answered.** `trace tb.u.bus[3]` and `trace tb.u.s.field`
do not resolve, because the only answer available would be what drives the
whole object, and that answer would look exactly like a correct one. The whole
object is answerable, and a query about it unions the statements that assign
its members — which is what slang's overlap rule gives for a whole-object
prefix. `questa_dbg.rs` pins the refusal so that making the path lookup lenient
has to fail a test first.

The other gaps against that model, none of which produce a wrong answer today:
the port flag says "a boundary was crossed" without saying which direction; an
initializer (`wire w = a & b`) is reported as the continuous assignment it
elaborates to; and a driver's source is visible only through the statement text
and the `#tag#` in `raw_kind` rather than as its own field.

## Answers vsim does not give

`M` is not a defect count. vsim declines to answer questions it is asked, and
rwave, reading the database rather than a rendered view, answers them. What
remains under `M` is one class, checked against the RTL:

| class | why vsim is silent | checked against |
|:--|:--|:--|
| a port driven or read from the scope around it | `find drivers -possible` does not follow a port, which is the whole point of the feature | `dut.sv:102`, the `initial` block driving `b.data`; `tinyriscv.v:361`, `.raddr_o(clint_raddr_o)`; `ifu_mem_ctl.sv:418`, the `always_comb` feeding `miss_state_ff`'s `din` at 456, and `:461`, the `assign` reading the same net |

Two more used to be counted here and are now settled in the comparison itself,
because both were the harness measuring a difference in what each tool
enumerates rather than a disagreement:

- **a declared object as an endpoint** — a clocked memory is a load of its
  clock, and on a design of behavioural cells `readers` lists every pin on the
  net. Both sides are filtered to statements and what was dropped is counted
  as `K`.
- **a statement that reads what it writes** — the hold path of `if (en) q <= d`
  reads `q`. `readers` omits it, NPI reports it, and matching NPI is the
  target, so it is waived wherever the load is the signal's own driver. All ten
  of picorv32's were checked to be exactly that before the rule was widened.

Every difference is printed, including the ones only counted before: a count
says how many to look at without saying which, and the ten above turned out to
be one class rather than ten claims.

## Run it on a design nobody shaped

A hand-written fixture tests whether a decoding rule is right. It cannot test
whether a table was read at all, because the fixture has nothing in the tables
the reader ignores. Every one of these was found by running the check over a
whole SoC, and none of them shows up on `dut.sv`, picorv32 or tinyriscv — all
three answer with nothing missing both before and after each fix:

| what the reader had wrong | how it looked |
|:--|:--|
| `inst_tbl` taken as the list of instances; it names about half of them | the answer stopped one level above the `always_ff` and came back as port hops |
| a statement inside a generate block is spelled with the block by `shape_tbl` and without it by `rw_process_tbl` | every statement in every generate block unreachable from the statement view |
| the walk to the enclosing statement stopped below the module, not at the statement | a generate block reported in place of the `always_ff` inside it |
| `PROCESS-CAUTION` and `PROCESS-MEMACESS` are statements too | as above, for those |
| `shape_tbl` holds a node only where elaboration needed one — on a large design, half the statements have none | those statements dropped entirely, though `rw_process_tbl` gives their file and line |
| a bus can arrive at a leaf module with no `port_tbl` row of its own | the walk stopped at the last instance that had one |
| a struct or array is recorded a member at a time, and the whole-object row is empty | nothing found for the object |
| the reader/writer lists are Tcl, so an element with a bit-select is braced | every vector reference in the module dropped |
| a `for` generate replicates a statement and each branch is a separate shape | all branches refused as an ambiguous name |
| `signal_tbl` carries a second pair of columns naming the primitive under the statement | the loop header reported instead of the assignment |

The differential is the only thing that finds these: each one produces a
confident wrong answer, not an error.

## Cross-backend parity

The same `dut.sv` builds under VCS with `+define+FSDB`, which produces
`tb.fsdb`. Running `rwave trace` against the FSDB and against the WLF and
comparing the two answers is the direct test of "the WLF backend agrees with
the NPI one" — a stronger statement than either backend matching its own vendor
tool.
