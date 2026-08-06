// End-to-end behavior of signal selection: --scope, --depth, --filter, and
// --exclude, as a user meets them on the command line.
//
// The motivating case is a CDC synchronizer whose *instance* is named after the
// signal it synchronizes (`u_bcm21_sync_<sig>`), a widespread RTL convention.
// When --filter matched the whole hierarchical path, asking for one status bit
// also dragged in that instance's clocks and pipeline flops, which on a real
// trace outweigh the wanted signal by orders of magnitude.
//
// The gates compose per alias path, so these tests care as much about which
// *rows* come back as which signals do.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// CDC-shaped VCD. `top.status` is the wanted status bit; the synchronizer
/// instance `top.u_bcm21_sync_status` is named after it and holds three nets,
/// one of which (`d_p`, VCD id `!`) is the *same net* as `top.status` and hence
/// an alias of it. `top.u_dma.status_q` is an unrelated signal carrying the
/// name.
///
///   #0  status=0 clk_d=0 q_p=0 status_q=0
///   #10 clk_d 0->1
///   #20 clk_d 1->0, status 0->1 (and its alias d_p with it)
const CDC_VCD: &str = "\
$timescale 1ns $end
$scope module top $end
$var wire 1 ! status $end
$scope module u_bcm21_sync_status $end
$var wire 1 ! d_p $end
$var wire 1 \" clk_d $end
$var wire 1 # q_p $end
$upscope $end
$scope module u_dma $end
$var wire 1 $ status_q $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
0\"
0#
0$
#10
1\"
#20
0\"
1!
";

/// Three levels of hierarchy with repeated instance names, so a bare `cnt` is
/// ambiguous and depth has something to cut: `root.{u_m0,u_m1}.{u_a,u_b}.cnt`,
/// plus a signal at each level to measure depth against.
const DEEP_VCD: &str = "\
$timescale 1ns $end
$scope module root $end
$var wire 1 ! clk $end
$scope module u_m0 $end
$var wire 1 \" m0_en $end
$scope module u_a $end
$var wire 4 # cnt [3:0] $end
$upscope $end
$scope module u_b $end
$var wire 4 $ cnt [3:0] $end
$upscope $end
$upscope $end
$scope module u_m1 $end
$var wire 1 % m1_en $end
$scope module u_a $end
$var wire 4 & cnt [3:0] $end
$upscope $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
0\"
b0001 #
b0000 $
b0000 &
#10
1!
";

/// A VCD escaped identifier whose name contains dots: `\\foo.bar` in scope `tb`.
/// Splitting a path on its last separator would call this signal `bar` and put
/// it a level deeper than it is.
const ESCAPED_VCD: &str = "\
$timescale 1ns $end
$scope module tb $end
$scope module u_inner $end
$var wire 1 ! \\foo.bar $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
1!
";

/// Write `body` to a uniquely named temp file (parallel-test safe) and return
/// its path. The caller removes it when done.
fn fixture(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("rwave_sel_{name}.vcd"));
    let mut f = std::fs::File::create(&p).expect("create tmp vcd");
    f.write_all(body.as_bytes()).expect("write vcd");
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_stderr(vcd: &std::path::Path, args: &[&str]) -> String {
    let out = run(vcd, args);
    assert!(
        !out.status.success(),
        "rwave {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The paths in a `list` result, one per row.
fn rows(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

// -- --filter: leaf names -----------------------------------------------------

#[test]
fn filter_names_a_signal_not_the_scope_named_after_it() {
    let vcd = fixture("motivating", CDC_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--filter", "status"]));
    // The status bit, its alias inside the synchronizer, and the unrelated
    // signal that also carries the name — but none of the synchronizer's own
    // nets, which is the whole point.
    assert!(got.contains(&"top.status".to_string()), "{got:?}");
    assert!(got.contains(&"top.u_dma.status_q".to_string()), "{got:?}");
    assert!(!got.iter().any(|p| p.ends_with("clk_d")), "{got:?}");
    assert!(!got.iter().any(|p| p.ends_with("q_p")), "{got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_dotted_filter_still_matches_the_whole_path() {
    let vcd = fixture("dotted", CDC_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--filter", "u_bcm21_sync_status."]));
    assert!(got.iter().any(|p| p.ends_with("clk_d")), "{got:?}");
    assert!(got.iter().any(|p| p.ends_with("q_p")), "{got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn filter_shows_every_alias_of_a_selected_signal() {
    // --filter says which signals are wanted; a signal's other paths are
    // information, not noise, so they stay.
    let vcd = fixture("aliasrows", CDC_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--filter", "status"]));
    assert!(
        got.contains(&"top.u_bcm21_sync_status.d_p".to_string()),
        "the alias of top.status is still listed: {got:?}"
    );
    let _ = std::fs::remove_file(&vcd);
}

// -- --exclude ----------------------------------------------------------------

#[test]
fn exclude_drops_a_subtree_and_spares_the_rest() {
    let vcd = fixture("subtree", CDC_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--exclude", "u_bcm21_sync_status."]));
    assert_eq!(got, vec!["top.status", "top.u_dma.status_q"], "{got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn excluding_a_synchronizer_never_costs_the_status_bit() {
    // `top.status` and the synchronizer's `d_p` are one net. Excluding the
    // instance must hide that path without losing the signal — per-signal
    // exclusion would drop the very bit the user is chasing.
    let vcd = fixture("aliassurvive", CDC_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--exclude", "*_sync_*.*"]));
    assert!(got.contains(&"top.status".to_string()), "clean path survives: {got:?}");
    assert!(!got.iter().any(|p| p.ends_with("d_p")), "excluded path hidden: {got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn exclude_applies_to_the_value_commands() {
    let vcd = fixture("values", CDC_VCD);
    for cmd in [
        vec!["summary"],
        vec!["dump", "--begin", "0", "--end", "40ns"],
        vec!["snapshot", "--at", "30ns"],
        vec!["compare", "--at", "0,30ns"],
    ] {
        let mut args = cmd.clone();
        args.extend_from_slice(&["--exclude", "u_bcm21_sync_status."]);
        let out = ok_stdout(&vcd, &args);
        assert!(!out.contains("clk_d"), "{cmd:?} still shows a synchronizer net:\n{out}");
        assert!(out.contains("top.status"), "{cmd:?} lost the status bit:\n{out}");
    }
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn exclude_wins_over_filter() {
    let vcd = fixture("excludewins", CDC_VCD);
    let got = rows(&ok_stdout(
        &vcd,
        &["list", "--filter", "status", "--exclude", "status_q"],
    ));
    assert!(!got.iter().any(|p| p.ends_with("status_q")), "{got:?}");
    assert!(got.contains(&"top.status".to_string()), "{got:?}");
    let _ = std::fs::remove_file(&vcd);
}

// -- --scope and --depth ------------------------------------------------------

#[test]
fn scope_selects_a_subtree_by_instance_name() {
    let vcd = fixture("scopeinst", DEEP_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "u_m0"]));
    assert_eq!(
        got,
        vec!["root.u_m0.m0_en", "root.u_m0.u_a.cnt[3:0]", "root.u_m0.u_b.cnt[3:0]"],
        "the scope and its descendants, nothing else: {got:?}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn scope_takes_a_dotted_suffix_and_a_wildcard() {
    let vcd = fixture("scopeforms", DEEP_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "u_m0.u_a"]));
    assert_eq!(got, vec!["root.u_m0.u_a.cnt[3:0]"], "{got:?}");
    // The same subtree named from the root.
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "root.u_m0.u_a"]));
    assert_eq!(got, vec!["root.u_m0.u_a.cnt[3:0]"], "{got:?}");
    // A wildcard over instance names.
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "u_m?"]));
    assert_eq!(got.len(), 5, "both mid instances and their children: {got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn depth_counts_from_the_scope_root() {
    let vcd = fixture("depth", DEEP_VCD);
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "u_m0", "--depth", "1"]));
    assert_eq!(got, vec!["root.u_m0.m0_en"], "only the scope's own signals: {got:?}");
    let got = rows(&ok_stdout(&vcd, &["list", "--scope", "u_m0", "--depth", "2"]));
    assert_eq!(got.len(), 3, "one level of children too: {got:?}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn depth_without_a_scope_is_a_usage_error() {
    let vcd = fixture("depthusage", DEEP_VCD);
    let out = run(&vcd, &["list", "--depth", "1"]);
    assert_eq!(out.status.code(), Some(2), "usage errors exit 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--depth requires --scope"), "{err}");
    let out = run(&vcd, &["list", "--scope", "u_m0", "--depth", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("depth must be positive"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_selection_matching_nothing_is_an_empty_result_not_an_error() {
    let vcd = fixture("nomatch", DEEP_VCD);
    for args in [
        vec!["list", "--scope", "no_such_scope"],
        vec!["list", "--filter", "no_such_signal"],
    ] {
        let out = run(&vcd, &args);
        assert!(out.status.success(), "{args:?} should exit 0");
        assert!(rows(&String::from_utf8_lossy(&out.stdout)).is_empty(), "{args:?}");
    }
    let _ = std::fs::remove_file(&vcd);
}

// -- escaped identifiers ------------------------------------------------------

#[test]
fn an_escaped_identifier_keeps_its_dots() {
    // The leaf is `\foo.bar`, so `bar` finds it and the scope name does not.
    // Its depth is 1 below u_inner, even though the path holds three dots.
    let vcd = fixture("escaped", ESCAPED_VCD);
    let path = rows(&ok_stdout(&vcd, &["list"]))[0].clone();
    assert!(path.contains("foo.bar"), "{path}");

    assert_eq!(rows(&ok_stdout(&vcd, &["list", "--filter", "bar"])), vec![path.clone()]);
    assert!(rows(&ok_stdout(&vcd, &["list", "--filter", "u_inner"])).is_empty());
    assert_eq!(
        rows(&ok_stdout(&vcd, &["list", "--scope", "u_inner", "--depth", "1"])),
        vec![path],
        "depth is measured on the hierarchy, not by counting dots"
    );
    let _ = std::fs::remove_file(&vcd);
}

// -- search -------------------------------------------------------------------

#[test]
fn selection_narrows_an_ambiguous_condition_name() {
    let vcd = fixture("searchnarrow", DEEP_VCD);
    let err = err_stderr(&vcd, &["search", "--condition", "cnt=1"]);
    assert!(err.contains("matches 3 signals"), "{err}");
    assert!(err.contains("--scope"), "the error names the way out: {err}");

    let out = ok_stdout(&vcd, &["search", "--condition", "cnt=1", "--scope", "u_m0.u_a"]);
    assert!(out.contains("root.u_m0.u_a.cnt[3:0]=1"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn search_says_when_the_selection_hid_the_signal() {
    let vcd = fixture("searchhidden", DEEP_VCD);
    let err = err_stderr(
        &vcd,
        &["search", "--condition", "cnt=1", "--scope", "u_m0.u_a", "--exclude", "cnt"],
    );
    assert!(err.contains("matches no signals"), "{err}");
    assert!(err.contains("within the current selection"), "{err}");
    assert!(err.contains("--exclude"), "it names which options applied: {err}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn an_exact_full_path_bypasses_the_selection() {
    // Naming a signal in full is an explicit choice; a broad --exclude (or one
    // inherited from a batch line) must not put it out of reach.
    let vcd = fixture("searchexact", DEEP_VCD);
    let out = ok_stdout(
        &vcd,
        &[
            "search",
            "--condition",
            "root.u_m0.u_a.cnt[3:0]=1",
            "--exclude",
            "cnt",
            "--scope",
            "u_m1",
        ],
    );
    assert!(out.contains("root.u_m0.u_a.cnt[3:0]=1"), "{out}");
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn search_show_is_narrowed_too() {
    let vcd = fixture("searchshow", DEEP_VCD);
    let out = ok_stdout(
        &vcd,
        &["search", "--condition", "root.u_m0.u_a.cnt[3:0]=1", "--show", "cnt", "--scope", "u_m0"],
    );
    assert!(out.contains("root.u_m0.u_a.cnt"), "{out}");
    assert!(out.contains("root.u_m0.u_b.cnt"), "{out}");
    assert!(!out.contains("u_m1"), "--show did not reach outside the scope:\n{out}");
    let _ = std::fs::remove_file(&vcd);
}

// -- batch --------------------------------------------------------------------

fn run_batch(vcd: &std::path::Path, global: &[&str], stdin_data: &str) -> String {
    let mut argv: Vec<&str> = vec!["--batch", vcd.to_str().unwrap()];
    argv.extend_from_slice(global);
    let mut child = Command::new(rwave())
        .args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn rwave");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_data.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("run batch");
    assert_eq!(out.status.code(), Some(0), "batch exits 0");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn batch_selection_defaults_apply_and_lines_override() {
    let vcd = fixture("batchdefaults", CDC_VCD);
    let out = run_batch(
        &vcd,
        &["--json", "--exclude", "u_bcm21_sync_status."],
        "list\nlist --exclude u_dma.\n",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(!lines[0].contains("clk_d"), "default applies: {}", lines[0]);
    assert!(lines[1].contains("clk_d"), "line replaces the default: {}", lines[1]);
    assert!(!lines[1].contains("status_q"), "line's own exclude applies: {}", lines[1]);
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_batch_line_lifts_an_inherited_filter_with_an_empty_value() {
    let vcd = fixture("batchescape", CDC_VCD);
    let out = run_batch(&vcd, &["--json", "--filter", "status"], "list\nlist --filter ''\n");
    let lines: Vec<&str> = out.lines().collect();
    assert!(!lines[0].contains("clk_d"), "default filter applies: {}", lines[0]);
    assert!(lines[1].contains("clk_d"), "empty value lifts it: {}", lines[1]);
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_batch_default_filter_now_narrows_search_lines() {
    // The documented behavior change: search used to ignore --filter.
    let vcd = fixture("batchsearch", DEEP_VCD);
    let out = run_batch(
        &vcd,
        &["--json", "--filter", "m0_en"],
        "search --condition cnt=1\nsearch --condition cnt=1 --filter ''\n",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("\"ok\":false"), "narrowed away: {}", lines[0]);
    assert!(lines[0].contains("within the current selection"), "{}", lines[0]);
    assert!(
        lines[1].contains("matches 3 signals"),
        "lifting the default restores the old ambiguity: {}",
        lines[1]
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_line_inheriting_depth_without_a_scope_fails_alone() {
    // Per-line isolation: the bad line reports, the batch carries on.
    let vcd = fixture("batchdepth", DEEP_VCD);
    let out = run_batch(&vcd, &["--json", "--depth", "1"], "list\nlist --scope u_m0\n");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("\"ok\":false"), "{}", lines[0]);
    assert!(lines[0].contains("--depth requires --scope"), "{}", lines[0]);
    assert!(lines[1].contains("\"ok\":true"), "the next line still runs: {}", lines[1]);
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn a_batch_line_matches_the_equivalent_single_command_byte_for_byte() {
    let vcd = fixture("batchidentical", DEEP_VCD);
    let file = vcd.to_str().unwrap();
    for args in [
        vec!["list", "--scope", "u_m0", "--depth", "1"],
        vec!["summary", "--scope", "u_m0", "--exclude", "cnt"],
        vec!["list", "--filter", "cnt", "--exclude", "u_m1."],
    ] {
        let mut single_argv = vec![args[0], "--json", file];
        single_argv.extend_from_slice(&args[1..]);
        let single = Command::new(rwave()).args(&single_argv).output().expect("spawn");
        let single = String::from_utf8_lossy(&single.stdout);
        let line = format!("{}\n", args.join(" "));
        let batch = run_batch(&vcd, &["--json"], &line);
        let expected =
            format!("{{\"id\":\"1\",\"ok\":true,\"result\":{}}}", single.trim_end());
        assert_eq!(batch.trim_end(), expected, "for {args:?}");
    }
    let _ = std::fs::remove_file(&vcd);
}
