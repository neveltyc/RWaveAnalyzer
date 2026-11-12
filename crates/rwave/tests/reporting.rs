// What a query says about itself: which signals it caught, which window it
// resolved, and why it came back empty: a same-prefixed strobe must not pass
// for the wanted signal, and an empty window must say whether the time or the
// trace is the reason.

use std::io::Write;
use std::process::{Command, Output};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// `DtsmTrainVal0Min` is the wanted 16-bit signal; `DtsmTrainVal0Min_strobe`
/// shares its prefix and never moves; `DtsmTrainVal0MinCopy` is a *second
/// declared name for the same net* (VCD id `!`), i.e. a true alias.
///
///   #0  Min=0 strobe=0 clk=0
///   #10 Min=0x5  clk 0->1
///   #20 Min=0xd  clk 1->0
///   #30 Min=0x1b clk 0->1
const VCD: &str = "\
$timescale 1ns $end
$scope module tb $end
$var wire 16 ! DtsmTrainVal0Min $end
$var wire 16 ! DtsmTrainVal0MinCopy $end
$var wire 1 \" DtsmTrainVal0Min_strobe $end
$var wire 1 # clk $end
$upscope $end
$enddefinitions $end
#0
b0 !
0\"
0#
#10
b101 !
1#
#20
b1101 !
0#
#30
b11011 !
1#
";

/// `never_dumped` is declared and never assigned: it appears in the hierarchy
/// with no value-change data behind it, the shape a signal has when it sits
/// outside the `$dumpvars` scope. `probe` is dumped once at #0 and then holds
/// still, which is the case it must not be confused with.
const NODUMP_VCD: &str = "\
$timescale 1ns $end
$scope module tb $end
$var wire 1 ! clk $end
$var wire 8 \" probe $end
$var wire 8 # never_dumped $end
$upscope $end
$enddefinitions $end
#0
0!
b0 \"
#10
1!
#20
0!
#30
1!
";

/// Write `body` to a per-test file under a per-process directory, published
/// with a rename.
///
/// A per-test name is not enough on its own: two `cargo test` runs share
/// `/tmp`, and `File::create` truncates before the write lands, so a
/// concurrent `rwave` can be handed zero bytes.
fn fixture(name: &str, body: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rwave_report_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let tmp = dir.join(format!("{name}.vcd.part"));
    let mut f = std::fs::File::create(&tmp).expect("create tmp vcd");
    f.write_all(body.as_bytes()).expect("write vcd");
    let p = dir.join(format!("{name}.vcd"));
    std::fs::rename(&tmp, &p).expect("publish fixture");
    p
}

fn run(vcd: &std::path::Path, args: &[&str]) -> Output {
    let mut argv: Vec<&str> = vec![args[0], vcd.to_str().unwrap()];
    argv.extend_from_slice(&args[1..]);
    Command::new(rwave()).args(&argv).output().expect("spawn rwave")
}

fn ok_stdout(vcd: &std::path::Path, args: &[&str]) -> String {
    let out = run(vcd, args);
    assert!(
        out.status.success(),
        "rwave {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

fn err_message(vcd: &std::path::Path, args: &[&str]) -> String {
    let out = run(vcd, args);
    assert!(!out.status.success(), "rwave {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ---------------------------------------------------------------- selection

/// A name that exists selects that signal alone; the same-prefixed strobe is
/// not pulled in. A fragment no signal is named after keeps substring reach,
/// and the header names everything it caught.
#[test]
fn a_prefix_collision_is_named_in_the_header() {
    let vcd = fixture("prefix", VCD);
    let out = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0Min"]);
    assert!(out.contains("Matched 1 signal: tb.DtsmTrainVal0Min"), "{out}");

    let out = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0"]);
    assert!(
        out.contains("Matched 2 signals: tb.DtsmTrainVal0Min, tb.DtsmTrainVal0Min_strobe"),
        "{out}"
    );

    let json = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0", "--json"]);
    assert!(json.contains(r#""matched":{"count":2,"paths":["#), "{json}");
    let _ = std::fs::remove_file(&vcd);
}

/// `--exact` gives the same answer for a name that exists.
#[test]
fn exact_narrows_to_the_named_signal() {
    let vcd = fixture("exact", VCD);
    let out = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0Min", "--exact"]);
    assert!(out.contains("Matched 1 signal: tb.DtsmTrainVal0Min"), "{out}");
    assert!(!out.contains("_strobe"), "{out}");

    // summary agrees, and so stops reporting the strobe's stillness as the
    // wanted signal's.
    let sum = ok_stdout(&vcd, &["summary", "--filter", "DtsmTrainVal0Min", "--exact"]);
    assert!(sum.contains("Active: 1, Static: 0"), "{sum}");
    let _ = std::fs::remove_file(&vcd);
}

/// A signal reached through one of its other names is labelled with the
/// canonical path in every row, so the report has to say which name matched.
#[test]
fn matching_through_an_alias_is_reported() {
    let vcd = fixture("alias", VCD);
    let out = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0MinCopy", "--exact"]);
    assert!(
        out.contains("Matched 1 signal: tb.DtsmTrainVal0MinCopy -> tb.DtsmTrainVal0Min"),
        "{out}"
    );
    assert!(out.contains("matched through an alias"), "{out}");

    let json = ok_stdout(&vcd, &["dump", "--filter", "DtsmTrainVal0MinCopy", "--exact", "--json"]);
    assert!(json.contains(r#""paths":["tb.DtsmTrainVal0MinCopy"]"#), "{json}");
    assert!(json.contains("matched through an alias"), "{json}");
    let _ = std::fs::remove_file(&vcd);
}

/// A whole-file query has no selection to report: no header in text, and
/// `matched: null` in JSON — present, because a key that comes and goes is a
/// key every caller has to guard.
#[test]
fn an_unfiltered_query_reports_no_match_header() {
    let vcd = fixture("nofilter", VCD);
    let out = ok_stdout(&vcd, &["dump"]);
    assert!(!out.contains("Matched"), "{out}");
    let json = ok_stdout(&vcd, &["dump", "--json"]);
    assert!(json.contains(r#""matched":null"#), "{json}");
    let _ = std::fs::remove_file(&vcd);
}

// ------------------------------------------------------------------- window

/// `dump` echoes the window it resolved, so a fractional time that landed on a
/// neighbouring tick — or nowhere near where it was meant to — is visible.
#[test]
fn dump_echoes_the_resolved_window() {
    let vcd = fixture("window", VCD);
    let out = ok_stdout(&vcd, &["dump", "--begin", "10ns", "--end", "20ns"]);
    assert!(out.contains("Window: 10ns..20ns"), "{out}");

    let json = ok_stdout(&vcd, &["dump", "--begin", "10ns", "--end", "20ns", "--json"]);
    assert!(json.contains(r#""begin_ticks":10"#), "{json}");
    assert!(json.contains(r#""end_ticks":20"#), "{json}");

    // A sub-tick value rounds, and the echo is where that shows.
    let json = ok_stdout(&vcd, &["dump", "--begin", "0.4ns", "--json"]);
    assert!(json.contains(r#""begin_ticks":0"#), "{json}");
    let _ = std::fs::remove_file(&vcd);
}

/// A signal can be in the hierarchy and absent from the value-change data —
/// outside `$dumpvars`, or never reached. "It never changed" and "it was never
/// dumped" are different answers with different fixes, and only one of them is
/// worth re-running a query for.
#[test]
fn never_dumped_is_not_reported_as_quiet() {
    let vcd = fixture("nodump", NODUMP_VCD);

    let out = ok_stdout(&vcd, &["dump", "--filter", "never_dumped", "--exact"]);
    assert!(out.contains("never written to the dump"), "{out}");
    assert!(!out.contains("no value changes"), "{out}");

    // A signal that *was* dumped but held still in the window is the other
    // answer, and must not borrow the first one's wording.
    let out = ok_stdout(
        &vcd,
        &["dump", "--filter", "probe", "--exact", "--begin", "5ns", "--end", "25ns"],
    );
    assert!(out.contains("no value changes in the window"), "{out}");
    assert!(!out.contains("never written to the dump"), "{out}");

    // A mixed selection says both halves.
    let out = ok_stdout(
        &vcd,
        &["dump", "--filter", "probe,never_dumped", "--begin", "5ns", "--end", "25ns"],
    );
    assert!(out.contains("no value changes in the window"), "{out}");
    assert!(out.contains("1 of the 2 selected signals"), "{out}");

    // summary reaches the same conclusion through the same helper.
    let sum = ok_stdout(&vcd, &["summary", "--filter", "never_dumped", "--exact"]);
    assert!(sum.contains("never written to the dump"), "{sum}");
    let _ = std::fs::remove_file(&vcd);
}

/// The four ways an empty result happens must not share one message.
#[test]
fn an_empty_result_says_which_kind_of_empty() {
    let vcd = fixture("empty", VCD);

    let past = ok_stdout(&vcd, &["dump", "--begin", "3321.60us"]);
    assert!(past.contains("after the last event at 30ns"), "{past}");

    let none = ok_stdout(&vcd, &["dump", "--filter", "zzznotasignal"]);
    assert!(none.contains("Matched 0 signals"), "{none}");
    assert!(none.contains("the selection matched no signals"), "{none}");

    let quiet = ok_stdout(&vcd, &["dump", "--begin", "11ns", "--end", "19ns"]);
    assert!(quiet.contains("no value changes in the window"), "{quiet}");
    assert!(!quiet.contains("never written to the dump"), "{quiet}");

    // All three reach a JSON caller through `hint`.
    for (args, needle) in [
        (vec!["dump", "--begin", "3321.60us", "--json"], "after the last event"),
        (vec!["dump", "--filter", "zzznotasignal", "--json"], "matched no signals"),
        (vec!["dump", "--begin", "11ns", "--end", "19ns", "--json"], "no value changes"),
    ] {
        let json = ok_stdout(&vcd, &args);
        assert!(json.contains("\"hint\":\""), "{json}");
        assert!(json.contains(needle), "{json}");
    }
    let _ = std::fs::remove_file(&vcd);
}

/// A window past the end leaves every signal holding its last value, which
/// reads as "nothing here ever moves" unless the command says otherwise.
#[test]
fn summary_flags_a_window_past_the_end() {
    let vcd = fixture("stale", VCD);
    let out = ok_stdout(&vcd, &["summary", "--begin", "3321.60us"]);
    assert!(out.contains("Active: 0, Static: 3"), "{out}");
    assert!(out.contains("after the last event at 30ns"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

/// A `--begin` past the trace is blamed on `--begin`, not on an `--end` the
/// user did not write.
#[test]
fn search_blames_the_flag_that_was_given() {
    let vcd = fixture("searchwin", VCD);
    let msg = err_message(&vcd, &["search", "--condition", "clk==1", "--begin", "3321.60us"]);
    assert!(msg.contains("--begin"), "{msg}");
    assert!(!msg.contains("end time must be"), "{msg}");

    // An explicit, genuinely inverted pair still reads as one.
    let msg = err_message(
        &vcd,
        &["search", "--condition", "clk==1", "--begin", "20ns", "--end", "10ns"],
    );
    assert!(msg.contains("end time must be >= begin time"), "{msg}");
    let _ = std::fs::remove_file(&vcd);
}

// ------------------------------------------------------------- applicability

/// A flag the command does not read is an error, not silence — which is what
/// makes a malformed value on it an error too.
#[test]
fn a_flag_the_command_ignores_is_rejected() {
    let vcd = fixture("applic", VCD);
    for args in [
        vec!["list", "--begin", "3321.60us"],
        vec!["list", "--begin", "banana"],
        vec!["snapshot", "--at", "0", "--begin", "1ns"],
        vec!["dump", "--at", "10ns"],
        vec!["info", "--filter", "clk"],
    ] {
        let msg = err_message(&vcd, &args);
        assert!(msg.contains("does not apply to"), "{args:?}: {msg}");
    }
    let _ = std::fs::remove_file(&vcd);
}

/// An `--at` past the last event still answers, with the final values, but
/// says so: `5s` typed for `5ns` must not pass as an ordinary answer.
#[test]
fn a_time_point_past_the_end_is_flagged() {
    let vcd = fixture("past_end", VCD);
    let out = ok_stdout(&vcd, &["snapshot", "--json", "--at", "5s", "--filter", "clk"]);
    assert!(out.contains("5s is after the last event at 30ns"), "{out}");
    let out = ok_stdout(&vcd, &["compare", "--at", "10ns,5s", "--filter", "clk"]);
    assert!(out.contains("Note: 5s is after the last event at 30ns"), "{out}");
    let out = ok_stdout(&vcd, &["snapshot", "--json", "--at", "20ns", "--filter", "clk"]);
    assert!(out.contains("\"hint\":null"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

/// `list` rows are alias paths; the header says paths and signals apart once
/// an alias makes them differ.
#[test]
fn the_list_header_counts_paths_and_signals_apart() {
    let vcd = fixture("list_header", VCD);
    let out = ok_stdout(&vcd, &["list"]);
    assert!(out.starts_with("Matched: 3/3 signals, 4 alias paths\n"), "{out}");
    let out = ok_stdout(&vcd, &["list", "--filter", "clk"]);
    assert!(out.starts_with("Matched: 1/3 signals\n"), "{out}");
    let out = ok_stdout(&vcd, &["list", "--verbose", "--filter", "clk"]);
    assert!(out.contains(" id="), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

/// `/` and `.` are interchangeable in a path pattern, for every name role.
#[test]
fn a_slash_path_reaches_a_dotted_hierarchy() {
    let vcd = fixture("slash", VCD);
    let out = ok_stdout(&vcd, &["list", "--filter", "tb/clk"]);
    assert!(out.contains("tb.clk"), "{out}");
    let out = ok_stdout(&vcd, &["search", "--condition", "tb/clk=1"]);
    assert!(out.contains("tb.clk=1"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

/// A name that matches nothing says how to find the right one.
#[test]
fn a_missing_name_points_at_list() {
    let vcd = fixture("missing", VCD);
    let msg = err_message(&vcd, &["search", "--condition", "clck=1"]);
    assert!(msg.contains("list --filter"), "{msg}");
    let _ = std::fs::remove_file(&vcd);
}

/// `--help --verbose` prints the compiled-in manual, front matter stripped.
#[test]
fn help_verbose_prints_the_manual() {
    for argv in [["--help", "--verbose"], ["--verbose", "-h"]] {
        let out = Command::new(rwave()).args(argv).output().expect("spawn rwave");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.starts_with("# rwave manual"), "{argv:?}: {}", &text[..80.min(text.len())]);
        assert!(!text.contains("name: waveform-debug"));
    }
    let out = Command::new(rwave()).arg("--help").output().expect("spawn rwave");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("rwave --help --verbose"));
    assert!(!text.contains("# rwave manual"));
}

const BUS_VCD: &str = "\
$timescale 1ns $end
$scope module top $end
$var wire 8 ! data [7:0] $end
$var wire 1 \" valid $end
$scope module u $end
$var wire 4 # cnt [3:0] $end
$upscope $end
$var wire 1 $ \\a/b  $end
$var wire 1 % \\a.b  $end
$upscope $end
$enddefinitions $end
#0
b0 !
0\"
b0 #
0$
0%
#10
b101 !
1\"
b11 #
1$
";

/// A glob, like a whole name, may leave the bus range off.
#[test]
fn a_glob_reaches_a_bus_without_its_range() {
    let vcd = fixture("glob_bus", BUS_VCD);
    let out = ok_stdout(&vcd, &["list", "--filter", "*data,*valid"]);
    assert!(out.contains("top.data[7:0]") && out.contains("top.valid"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

/// A full path bypasses the selection with or without its bus range, and a
/// literal spelling beats a separator-folded one.
#[test]
fn a_full_path_bypasses_the_selection_without_its_range() {
    let vcd = fixture("bypass", BUS_VCD);
    let out = ok_stdout(&vcd, &["search", "--scope", "u", "--condition", "top.data=5"]);
    assert!(out.contains("top.data[7:0]=5"), "{out}");
    let out = ok_stdout(&vcd, &["search", "--condition", "top.\\a/b=1"]);
    assert!(out.contains("Found: 1"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}
