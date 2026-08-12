// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Reads a real Questa debug database, when one is available.
//!
//! The synthetic fixtures in the unit tests pin the decoding rules; this pins
//! them against a database QuestaSim actually wrote, which is the only thing
//! that catches a column read correctly in theory and wrongly in practice. No
//! vendor file is committed, so the test asks for one:
//!
//! ```sh
//! RWAVE_TEST_DBG=/path/run.dbg RWAVE_TEST_LIB=/path/work cargo test -p rwave --test questa_dbg
//! ```
//!
//! Build the pair with `verify/questa/README.md`. Without the variables the
//! test reports that it was skipped and passes: CI has no QuestaSim, and a
//! silent no-op would look the same as a real run.

use std::path::PathBuf;

use rwave::backend::design::Direction;
use rwave::plugin::builtin::questa::dbg::Design;

fn fixture() -> Option<(PathBuf, PathBuf)> {
    let dbg = std::env::var_os("RWAVE_TEST_DBG")?;
    let lib = std::env::var_os("RWAVE_TEST_LIB")?;
    Some((PathBuf::from(dbg), PathBuf::from(lib)))
}

#[test]
fn a_real_database_answers_the_questions_questa_answers() {
    let Some((dbg, lib)) = fixture() else {
        eprintln!("skipped: set RWAVE_TEST_DBG and RWAVE_TEST_LIB to a Questa debug database");
        return;
    };
    let mut d = Design::open(&dbg, Some(&lib)).expect("open the design");

    // A port-crossing driver. `out` is core's output port, written in u_alu, so
    // answering it at all requires following the port into the child module —
    // and vsim answers this one with dut.sv:33.
    let hops = d.trace("/tb/u_core/out", Direction::Driver, false).unwrap();
    assert!(!hops.is_empty(), "no driver found for /tb/u_core/out");
    let d33 = hops
        .iter()
        .find(|h| h.line == Some(33))
        .unwrap_or_else(|| panic!("expected a driver at line 33, got {hops:?}"));
    assert_eq!(d33.scope, "tb.u_core.u_alu");
    assert_eq!(d33.file.as_deref(), Some("dut.sv"));
    assert!(d33.boundary, "reaching it crossed a port, and that is worth reporting");
    assert!(
        d33.signals.iter().any(|s| s.ends_with("res_q")),
        "the assignment reads res_q: {:?}",
        d33.signals
    );

    // The same net named from inside the module that drives it: same statement,
    // no boundary this time.
    let inner = d.trace("/tb/u_core/u_alu/res", Direction::Driver, false).unwrap();
    assert!(inner.iter().any(|h| h.line == Some(33) && h.scope == "tb.u_core.u_alu"));

    // A load. `out` is read by the accumulator's always_ff in core.
    let loads = d.trace("/tb/u_core/out", Direction::Load, false).unwrap();
    assert!(
        loads.iter().any(|h| h.scope == "tb.u_core" && h.file.as_deref() == Some("dut.sv")),
        "expected a load in tb.u_core with a source location, got {loads:?}"
    );
    assert!(
        loads.iter().all(|h| h.line.is_some()),
        "every load carries a line — this is what the vsim route could not do: {loads:?}"
    );

    // Control dependencies are recorded, so `--control` adds the clock and
    // reset rather than being refused.
    let plain = d.trace("/tb/u_core/u_alu/res_q", Direction::Driver, false).unwrap();
    let ctrl = d.trace("/tb/u_core/u_alu/res_q", Direction::Driver, true).unwrap();
    let plain_ops: usize = plain.iter().map(|h| h.signals.len()).sum();
    let ctrl_ops: usize = ctrl.iter().map(|h| h.signals.len()).sum();
    assert!(ctrl_ops > plain_ops, "--control must add the gating signals");
    assert!(
        ctrl.iter().any(|h| h.signals.iter().any(|s| s.ends_with("clk"))),
        "the clock is a control dependency of an always_ff: {ctrl:?}"
    );

    // A name the design does not have is not the same as a name with no
    // drivers, and the difference has to survive.
    assert!(!d.resolves("/tb/u_core/no_such_signal"));
    assert!(d.resolves("/tb/u_core/out"));

    // Part of an object is not an object this reader answers about, and the
    // refusal is the point rather than an omission. slang attributes every
    // driver to the longest static prefix of what it assigns, with the bit
    // range it covers; this database records neither, so a question about one
    // bit or one field can only be answered with what drives the whole thing —
    // which would be a wrong answer wearing a right one's clothes. Answering
    // the whole object when asked about a part is the failure this pins shut:
    // whoever later makes the path lookup lenient has to see this fail.
    for part in [
        "/tb/u_core/out[3]",
        "/tb/u_core/out[3:1]",
        "/tb/u_core/u_alu/res_q[0]",
    ] {
        assert!(!d.resolves(part), "{part} names part of an object, which is not answerable");
        assert!(d.trace(part, Direction::Driver, false).is_err(), "{part} must be refused");
    }
}
