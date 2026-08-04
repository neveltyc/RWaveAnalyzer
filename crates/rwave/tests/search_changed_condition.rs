// Behavior tests for the `changed(SIG)` condition term (event mode).
//
// `changed(SIG)` is an edge predicate: a clause containing one fires at
// exactly the ticks where SIG truly transitions (t=0 initialization and a
// signal's first definition are not transitions) while the clause's level
// terms hold on the post-update state. Any changed() term switches `search`
// to event mode; every OR clause must then carry one (mixing errors). Two
// changed() terms in one clause require both signals to transition at the
// same tick. With no `--show`, event mode defaults to showing the changed()
// signals. The legacy `--changed` flag is gone and points at the new syntax.

use std::io::Write;
use std::process::{Command, Output};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// Edge-test VCD under scope `tb`. Timeline (ns):
///   #0   req=0 ack=0 ready=0 state=0   (initialization — not a transition)
///   #10  ready 0->1
///   #15  req 0->1                       (ready=1)
///   #20  req 1->0, ack 0->1             (both transition on one tick; ready=1)
///   #30  req 0->1, ready 1->0           (post-update: ready=0 at 30)
///   #40  req 1->0, ack 1->0             (both transition again; ready=0)
///   #50  state 0->5
fn write_edge_vcd(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).expect("create tmp vcd");
    writeln!(f, "$timescale 1ns $end").unwrap();
    writeln!(f, "$scope module tb $end").unwrap();
    writeln!(f, "$var wire 1 ! req $end").unwrap();
    writeln!(f, "$var wire 1 # ack $end").unwrap();
    writeln!(f, "$var wire 1 % ready $end").unwrap();
    writeln!(f, "$var reg 8 & state $end").unwrap();
    writeln!(f, "$upscope $end").unwrap();
    writeln!(f, "$enddefinitions $end").unwrap();
    writeln!(f, "#0").unwrap();
    writeln!(f, "0!").unwrap();
    writeln!(f, "0#").unwrap();
    writeln!(f, "0%").unwrap();
    writeln!(f, "b0 &").unwrap();
    writeln!(f, "#10").unwrap();
    writeln!(f, "1%").unwrap();
    writeln!(f, "#15").unwrap();
    writeln!(f, "1!").unwrap();
    writeln!(f, "#20").unwrap();
    writeln!(f, "0!").unwrap();
    writeln!(f, "1#").unwrap();
    writeln!(f, "#30").unwrap();
    writeln!(f, "1!").unwrap();
    writeln!(f, "0%").unwrap();
    writeln!(f, "#40").unwrap();
    writeln!(f, "0!").unwrap();
    writeln!(f, "0#").unwrap();
    writeln!(f, "#50").unwrap();
    writeln!(f, "b101 &").unwrap();
}

/// Write the edge VCD to a uniquely named temp file (parallel-test safe) and
/// return its path; the caller removes it when done.
fn fixture(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rwave_chg_{name}.vcd"));
    write_edge_vcd(&p);
    p
}

/// Run `rwave search <vcd> <extra...>` and return the raw process output.
fn run(vcd: &std::path::Path, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = vec!["search", vcd.to_str().unwrap()];
    args.extend_from_slice(extra);
    Command::new(rwave()).args(&args).output().expect("spawn rwave")
}

/// Run and assert success, returning stdout.
fn ok_stdout(vcd: &std::path::Path, extra: &[&str]) -> String {
    let out = run(vcd, extra);
    assert!(
        out.status.success(),
        "rwave search {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run expecting failure, returning stderr.
fn err_stderr(vcd: &std::path::Path, extra: &[&str]) -> String {
    let out = run(vcd, extra);
    assert!(
        !out.status.success(),
        "rwave search {extra:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const WIN: &[&str] = &["--begin", "0", "--end", "100ns"];

#[test]
fn standalone_changed_is_event_mode() {
    // A lone changed(SIG) is a complete condition: event mode, one event per
    // true transition of req (15, 20, 30, 40), the t=0 init excluded, and
    // `--show` defaulting to the changed signal.
    let vcd = fixture("standalone");
    let out = ok_stdout(&vcd, &[&["--condition", "changed(req)"], WIN].concat());
    assert!(out.contains("Found: 4 event(s)"), "req transitions 4 times:\n{out}");
    for t in ["T=15ns", "T=20ns", "T=30ns", "T=40ns"] {
        assert!(out.contains(t), "missing {t}:\n{out}");
    }
    assert!(!out.contains("T=0"), "t=0 initialization is not a transition:\n{out}");
    assert!(out.contains("tb.req="), "default --show is the changed signal:\n{out}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn changed_with_level_term_evaluates_post_update() {
    // Level terms are judged on the state *after* the tick's updates land.
    // At 30 req rises while ready falls on the same tick: post-update ready=0,
    // so `changed(req),ready=1` skips 30 (fires 15, 20 only) and
    // `changed(req),ready=0` includes it (fires 30, 40).
    let vcd = fixture("post_update");
    let high = ok_stdout(&vcd, &[&["--condition", "changed(req),ready=1"], WIN].concat());
    assert!(
        high.contains("Found: 2 event(s)") && high.contains("T=15ns") && high.contains("T=20ns"),
        "ready=1 clause fires at 15/20 only:\n{high}"
    );
    let low = ok_stdout(&vcd, &[&["--condition", "changed(req),ready=0"], WIN].concat());
    assert!(
        low.contains("Found: 2 event(s)") && low.contains("T=30ns") && low.contains("T=40ns"),
        "ready=0 clause fires at 30/40 (post-update at 30):\n{low}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn two_changed_terms_require_same_tick() {
    // changed(req),changed(ack): both must transition on one tick — 20 and 40.
    let vcd = fixture("both");
    let out = ok_stdout(&vcd, &[&["--condition", "changed(req),changed(ack)"], WIN].concat());
    assert!(
        out.contains("Found: 2 event(s)") && out.contains("T=20ns") && out.contains("T=40ns"),
        "simultaneous transitions only:\n{out}"
    );
    // Default --show is the union of the changed signals.
    assert!(out.contains("tb.ack=") && out.contains("tb.req="), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn or_clauses_union_their_edges() {
    // changed(req) OR changed(state): the union of both signals' transitions.
    let vcd = fixture("or_edges");
    let out = ok_stdout(
        &vcd,
        &[&["--condition", "changed(req)", "--condition", "changed(state)"], WIN].concat(),
    );
    assert!(out.contains("Found: 5 event(s)"), "4 req edges + 1 state change:\n{out}");
    assert!(out.contains("T=50ns"), "state change at 50:\n{out}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn mixed_changed_and_level_clauses_rejected() {
    let vcd = fixture("mixed");
    let e = err_stderr(
        &vcd,
        &[&["--condition", "changed(req)", "--condition", "ready=1"], WIN].concat(),
    );
    assert!(e.contains("cannot mix changed()"), "targeted mixing error:\n{e}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn changed_flag_is_gone_with_pointer() {
    let vcd = fixture("flag_gone");
    let e = err_stderr(&vcd, &["--condition", "a=1", "--changed", "req"]);
    assert!(
        e.contains("--condition \"changed(SIG)\""),
        "removed flag points at the new syntax:\n{e}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn malformed_changed_terms_error() {
    let vcd = fixture("malformed");
    // Comma inside the parens is an AND split; guidance names the fix.
    let e = err_stderr(&vcd, &[&["--condition", "changed(req,ack)"], WIN].concat());
    assert!(e.contains("exactly one signal"), "{e}");
    // Trailing comparison.
    let e = err_stderr(&vcd, &[&["--condition", "changed(req)=1"], WIN].concat());
    assert!(e.contains("takes no comparison"), "{e}");
    // Empty.
    let e = err_stderr(&vcd, &[&["--condition", "changed()"], WIN].concat());
    assert!(e.contains("requires a signal"), "{e}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn event_json_shape() {
    // mode=event; `changed` echoes the path-sorted array of changed() signals;
    // condition echoes the terms as written; resolved uses full paths.
    let vcd = fixture("json");
    let out = ok_stdout(
        &vcd,
        &[&["--json", "--condition", "changed(req),changed(ack)"], WIN].concat(),
    );
    assert!(out.contains("\"mode\":\"event\""), "{out}");
    assert!(out.contains("\"changed\":[\"tb.ack\",\"tb.req\"]"), "path-sorted array:\n{out}");
    assert!(out.contains("\"condition\":\"changed(req),changed(ack)\""), "{out}");
    assert!(
        out.contains("\"condition_resolved\":\"changed(tb.req),changed(tb.ack)\""),
        "{out}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn interval_mode_unaffected() {
    // A level-only condition still yields intervals: ready=1 holds [10, 30).
    let vcd = fixture("interval");
    let out = ok_stdout(&vcd, &[&["--condition", "ready=1"], WIN].concat());
    assert!(
        out.contains("Found: 1 interval(s)") && out.contains("10ns") && out.contains("30ns"),
        "level-only search stays interval mode:\n{out}"
    );
    let _ = std::fs::remove_file(&vcd);
}
