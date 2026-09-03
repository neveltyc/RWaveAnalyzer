# Manual checks for `trace`

`verify/run.sh` cannot cover `trace`: it needs a Verdi install, a licence, and a
design database, none of which CI has. These are the checks to run by hand on a
machine that has them.

`dut.sv` is shaped to produce the cases that are easy to get wrong, not to be a
realistic design: a continuous assign, a procedural assign with async reset, an
`always_comb` case, an interface with a modport, a part-select, a three-level
hierarchy, a testbench that drives through the interface, and a free-running
counter whose single statement both writes and reads the same net.

## Build

```bash
source <your Verdi/VCS environment>
vcs -full64 -sverilog -kdb -debug_access+all -lca \
    -P "$VERDI_HOME/share/PLI/VCS/LINUX64/novas.tab" \
    "$VERDI_HOME/share/PLI/VCS/LINUX64/pli.a" \
    dut.sv -o simv
./simv                      # writes tb.fsdb
# rwave finds libNPI.so under $VERDI_HOME on its own; set RWAVE_FSDB_LIB only to
# override that (e.g. a non-standard layout):
# export RWAVE_FSDB_LIB="$VERDI_HOME/share/NPI/lib/linux64/libNPI.so"
```

The PLI path is `linux64` on some releases and `LINUX64` on others.

## Checks

| Command | Expected |
|---|---|
| `rwave trace tb.fsdb tb.u_core.u_alu.res` | one `assign` driver at `dut.sv`'s `assign res = res_q`, found with no `--kdb` |
| `rwave trace tb.fsdb tb.u_core.free_cnt` | `status: resolved`. A statement that both writes and reads its own net must not be reported as anything unusual |
| `rwave trace tb.fsdb tb.u_core.state --control` | more hops than without it, and the same `status` |
| `rwave trace tb.fsdb tb.u_core.state --load` | the `always_comb` reader and the `u_alu` port |
| `rwave trace tb.fsdb tb.u_core.u_alu.res --at 50ns` | each endpoint annotated with its value |
| `rwave trace tb.fsdb tb.u_core.u_alu.res --kdb /nowhere` | fails naming `--kdb`, and does not fall back to the recorded path |
| `rwave trace tb.fsdb no.such.signal` | reports that the name is not in the design database |
| two traces in one `--batch` session | the design loads once |

## Version compatibility

Verdi must be at least as new as the FSDB. Reading a newer dump fails with the
file's format version named. Worth re-checking on a Verdi predating
`npi_waveform_info` (2018 does): the waveform commands must keep working, and
`trace` must ask for `--kdb` rather than failing, since it cannot then read the
design library path out of the header.

## Note

`npi_init` creates a `<argv0>Log/` directory in the working directory. That is
NPI's own behaviour, not rwave's.
