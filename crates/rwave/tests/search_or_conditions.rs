// Behavior tests for multi-clause OR on `search --condition` (OR-of-ANDs).
//
// A single `--condition` is a comma-separated AND list (unchanged). Repeating
// `--condition` ORs the clauses: a time satisfies the search when *any* clause
// holds. OR only changes which moments count as satisfied — it does not change
// the interval / segment / event row shapes. Clauses that are identical, only
// term-order-permuted, or alias-equivalent fold to one (silent de-dup); the
// same value written in different bases does not fold.

use std::io::Write;
use std::process::{Command, Output};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// A VCD with two structurally identical channels plus `err`, `state`, `data`,
/// under scope `tb`. Timeline (ns): ch0 handshakes in [10,20), ch1 in [30,40),
/// `err=1` in [50,60), `state=5` in [70,80); `data` steps 0 -> 0xAA (at 15) ->
/// 0xBB (at 35) so a change lands inside each channel's handshake window.
fn write_chan_vcd(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).expect("create tmp vcd");
    writeln!(f, "$timescale 1ns $end").unwrap();
    writeln!(f, "$scope module tb $end").unwrap();
    writeln!(f, "$var wire 1 ! ch0_valid $end").unwrap();
    writeln!(f, "$var wire 1 # ch0_ready $end").unwrap();
    writeln!(f, "$var wire 1 % ch1_valid $end").unwrap();
    writeln!(f, "$var wire 1 & ch1_ready $end").unwrap();
    writeln!(f, "$var wire 1 * err $end").unwrap();
    writeln!(f, "$var reg 8 ( state $end").unwrap();
    writeln!(f, "$var reg 8 ) data $end").unwrap();
    writeln!(f, "$upscope $end").unwrap();
    writeln!(f, "$enddefinitions $end").unwrap();
    writeln!(f, "#0").unwrap();
    for s in ["0!", "0#", "0%", "0&", "0*"] {
        writeln!(f, "{s}").unwrap();
    }
    writeln!(f, "b0 (").unwrap();
    writeln!(f, "b0 )").unwrap();
    writeln!(f, "#10").unwrap(); // ch0 handshake begins
    writeln!(f, "1!").unwrap();
    writeln!(f, "1#").unwrap();
    writeln!(f, "#15").unwrap(); // data step inside ch0 window
    writeln!(f, "b10101010 )").unwrap();
    writeln!(f, "#20").unwrap(); // ch0 handshake ends
    writeln!(f, "0!").unwrap();
    writeln!(f, "0#").unwrap();
    writeln!(f, "#30").unwrap(); // ch1 handshake begins
    writeln!(f, "1%").unwrap();
    writeln!(f, "1&").unwrap();
    writeln!(f, "#35").unwrap(); // data step inside ch1 window
    writeln!(f, "b10111011 )").unwrap();
    writeln!(f, "#40").unwrap(); // ch1 handshake ends
    writeln!(f, "0%").unwrap();
    writeln!(f, "0&").unwrap();
    writeln!(f, "#50").unwrap(); // err pulse begins
    writeln!(f, "1*").unwrap();
    writeln!(f, "#60").unwrap(); // err pulse ends
    writeln!(f, "0*").unwrap();
    writeln!(f, "#70").unwrap(); // state == 5
    writeln!(f, "b101 (").unwrap();
    writeln!(f, "#80").unwrap();
    writeln!(f, "b0 (").unwrap();
}

/// Write the channel VCD to a uniquely named temp file (parallel-test safe) and
/// return its path; the caller removes it when done.
fn fixture(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rwave_or_{name}.vcd"));
    write_chan_vcd(&p);
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

/// Extract a `"key":"..."` string value from a compact JSON line.
fn json_str_field(json: &str, key: &str) -> String {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle).unwrap_or_else(|| panic!("no {key} in {json}")) + needle.len();
    let rest = &json[start..];
    let end = rest.find('"').expect("unterminated json string");
    rest[..end].to_string()
}

// --- union semantics (the core feature) -------------------------------------

#[test]
fn or_interval_is_union_of_clauses() {
    // #2/#3: each channel alone yields exactly one handshake interval; ORing the
    // two clauses yields BOTH windows (the "any channel" use case). A third
    // clause adds the err window. Union, not intersection.
    let vcd = fixture("interval_union");
    let win = &["--begin", "0", "--end", "100ns"];

    // Each channel alone yields exactly its own window and NOT the other's.
    let only0 = ok_stdout(&vcd, &[&["--condition", "ch0_valid=1,ch0_ready=1"], &win[..]].concat());
    assert!(
        only0.contains("Found: 1 interval(s)") && only0.contains("..20ns") && !only0.contains("..40ns"),
        "ch0 alone is [10,20) only:\n{only0}"
    );
    let only1 = ok_stdout(&vcd, &[&["--condition", "ch1_valid=1,ch1_ready=1"], &win[..]].concat());
    assert!(
        only1.contains("Found: 1 interval(s)") && only1.contains("..40ns") && !only1.contains("..20ns"),
        "ch1 alone is [30,40) only:\n{only1}"
    );

    // ORing the two clauses yields BOTH windows — neither single clause does.
    let both = ok_stdout(
        &vcd,
        &[
            &["--condition", "ch0_valid=1,ch0_ready=1", "--condition", "ch1_valid=1,ch1_ready=1"],
            &win[..],
        ]
        .concat(),
    );
    assert!(
        both.contains("Found: 2 interval(s)")
            && both.contains("10ns        ..20ns")
            && both.contains("30ns        ..40ns"),
        "ch0 OR ch1 must union both windows:\n{both}"
    );

    let three = ok_stdout(
        &vcd,
        &[
            &[
                "--condition",
                "ch0_valid=1,ch0_ready=1",
                "--condition",
                "ch1_valid=1,ch1_ready=1",
                "--condition",
                "err=1",
            ],
            &win[..],
        ]
        .concat(),
    );
    assert!(
        three.contains("Found: 3 interval(s)") && three.contains("50ns") && three.contains("60ns"),
        "third clause adds the err window:\n{three}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn or_clause_can_use_ne_independently() {
    // #15: `!=` semantics are per-clause and unchanged. `err!=0` is a distinct
    // clause from the ch0 handshake; the result is their union.
    let vcd = fixture("ne_clause");
    let out = ok_stdout(
        &vcd,
        &[
            "--condition", "ch0_valid=1,ch0_ready=1",
            "--condition", "err!=0",
            "--begin", "0", "--end", "100ns",
        ],
    );
    assert!(
        out.contains("Found: 2 interval(s)") && out.contains("10ns") && out.contains("50ns"),
        "ch0 handshake OR err!=0:\n{out}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn or_segment_mode_splits_within_union_windows() {
    // #13: in segment mode the union windows are split by the observed --show
    // value. ch0_valid=1 OR ch1_valid=1 covers [10,20) and [30,40); `data`
    // steps inside each, so each window splits into two segments.
    let vcd = fixture("segment_union");
    let out = ok_stdout(
        &vcd,
        &[
            "--condition", "ch0_valid=1",
            "--condition", "ch1_valid=1",
            "--show", "data",
            "--begin", "0", "--end", "100ns",
        ],
    );
    assert!(
        out.contains("Found: 4 segment(s)") && out.contains("0xaa") && out.contains("0xbb"),
        "union windows split by data value:\n{out}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn or_event_mode_fires_per_clause() {
    // #14: event mode fires when a clause's changed() signal truly transitions
    // AND that clause's level terms hold. `data` changes at 15ns (inside ch0's
    // handshake) and 35ns (inside ch1's) — one event enabled by each clause.
    let vcd = fixture("event_union");
    let out = ok_stdout(
        &vcd,
        &[
            "--condition", "changed(data),ch0_valid=1,ch0_ready=1",
            "--condition", "changed(data),ch1_valid=1,ch1_ready=1",
            "--begin", "0", "--end", "100ns",
        ],
    );
    assert!(
        out.contains("Found: 2 event(s)") && out.contains("15ns") && out.contains("35ns"),
        "one event enabled by each clause:\n{out}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn or_limit_caps_merged_results() {
    // #18: --limit bounds the final merged result set, semantics unchanged.
    let vcd = fixture("limit");
    let clauses = [
        "--condition", "ch0_valid=1,ch0_ready=1",
        "--condition", "ch1_valid=1,ch1_ready=1",
        "--condition", "err=1",
    ];
    let capped = ok_stdout(
        &vcd,
        &[&clauses[..], &["--limit", "2", "--begin", "0", "--end", "100ns"]].concat(),
    );
    assert!(
        capped.contains("Found: 3+ interval(s)")
            && capped.contains("TRUNCATED: showing 2 of 3+ intervals")
            && capped.contains("--limit"),
        "--limit 2 shows 2 of 3 and says so, naming the flag that lifts the cap:\n{capped}"
    );
    let all = ok_stdout(
        &vcd,
        &[&clauses[..], &["--limit", "0", "--begin", "0", "--end", "100ns"]].concat(),
    );
    assert!(all.contains("Found: 3 interval(s)"), "--limit 0 shows all 3:\n{all}");
    let _ = std::fs::remove_file(&vcd);
}

// --- condition echo (PRD §4) ------------------------------------------------

#[test]
fn single_clause_echo_has_no_parens() {
    // A lone --condition echoes exactly as today — no parens, no ` OR `.
    let vcd = fixture("echo_single");
    let out = ok_stdout(&vcd, &["--json", "--condition", "ch0_valid=1,ch0_ready=1"]);
    assert_eq!(json_str_field(&out, "condition"), "ch0_valid=1,ch0_ready=1");
    assert_eq!(
        json_str_field(&out, "condition_resolved"),
        "tb.ch0_valid=1,tb.ch0_ready=1"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn multi_clause_echo_is_parenthesized_or() {
    // Multiple clauses: each parenthesized, joined by ` OR `, in command-line
    // order — for both the raw `condition` and the resolved `condition_resolved`.
    let vcd = fixture("echo_multi");
    let out = ok_stdout(
        &vcd,
        &["--json", "--condition", "ch0_valid=1,ch0_ready=1", "--condition", "err=1"],
    );
    assert_eq!(
        json_str_field(&out, "condition"),
        "(ch0_valid=1,ch0_ready=1) OR (err=1)"
    );
    assert_eq!(
        json_str_field(&out, "condition_resolved"),
        "(tb.ch0_valid=1,tb.ch0_ready=1) OR (tb.err=1)"
    );
    let _ = std::fs::remove_file(&vcd);
}

// --- de-duplication (PRD §7) ------------------------------------------------

#[test]
fn duplicate_clauses_fold_to_one() {
    // #4 identical, #5 term-order permuted, #6 alias path: all fold to one
    // clause, echoed once without parens (first occurrence kept).
    let vcd = fixture("dedup");

    let identical =
        ok_stdout(&vcd, &["--json", "--condition", "ch0_valid=1", "--condition", "ch0_valid=1"]);
    assert_eq!(json_str_field(&identical, "condition"), "ch0_valid=1", "#4 identical");

    let swapped = ok_stdout(
        &vcd,
        &["--json", "--condition", "ch0_valid=1,ch0_ready=1", "--condition", "ch0_ready=1,ch0_valid=1"],
    );
    assert_eq!(
        json_str_field(&swapped, "condition"),
        "ch0_valid=1,ch0_ready=1",
        "#5 term-order keeps first occurrence"
    );

    let aliased = ok_stdout(
        &vcd,
        &["--json", "--condition", "ch0_valid=1", "--condition", "tb.ch0_valid=1"],
    );
    assert_eq!(
        json_str_field(&aliased, "condition"),
        "ch0_valid=1",
        "#6 different path to same signal folds"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn different_base_clauses_are_not_folded() {
    // #7: `state=5` and `state=0x5` are the same value but different spellings —
    // not folded (no cross-base normalization). Both echo; the result rows are
    // the same (both match state == 5 in [70,80)).
    let vcd = fixture("no_dedup_base");
    let out = ok_stdout(
        &vcd,
        &["--json", "--condition", "state=5", "--condition", "state=0x5", "--begin", "0", "--end", "100ns"],
    );
    assert_eq!(json_str_field(&out, "condition"), "(state=5) OR (state=0x5)");
    assert!(out.contains("\"begin_h\":\"70ns\""), "single matched window [70ns,80ns):\n{out}");
    assert!(out.contains("\"shown\":1"), "two identical-value clauses still match one interval:\n{out}");
    let _ = std::fs::remove_file(&vcd);
}

// --- error paths (unchanged from single-clause; PRD §5/§8) ------------------

#[test]
fn empty_clause_errors() {
    // #8: an empty --condition fails the whole command with today's text.
    let vcd = fixture("err_empty");
    let out = run(&vcd, &["--condition", ""]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("search requires --condition"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn bad_term_errors() {
    // #9: a malformed term fails with today's parse error.
    let vcd = fixture("err_term");
    let out = run(&vcd, &["--condition", "ch0_valid="]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid empty signal/value in condition"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn in_string_or_is_not_an_operator() {
    // #19: `OR` / `|` inside a --condition string are not logical operators;
    // they are ordinary text and trigger a parse error. OR is expressed only by
    // repeating --condition.
    let vcd = fixture("err_instr_or");
    for cond in ["ch0_valid=1 OR ch1_valid=1", "ch0_valid=1|ch1_valid=1"] {
        let out = run(&vcd, &["--condition", cond]);
        assert!(!out.status.success(), "{cond:?} should fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("invalid target"),
            "{cond:?} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn ambiguous_clause_still_errors() {
    // #11 / §8: the per-term unique-resolution requirement is not relaxed. A
    // clause whose signal pattern matches >1 signal still errors.
    let vcd = fixture("err_ambiguous");
    let out = run(&vcd, &["--condition", "ch0_valid=1", "--condition", "ch0=1"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("matches 2 signals"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn missing_condition_still_errors() {
    // #12: no --condition at all is the usual required-argument error.
    let vcd = fixture("err_missing");
    let out = run(&vcd, &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the following arguments are required: --condition"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&vcd);
}
