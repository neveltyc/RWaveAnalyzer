// Integration tests for `--batch` mode (commands/ / cli.rs / batch.rs).
//
// These spawn the built binary and drive commands on stdin, mirroring how a
// script or agent would use batch mode. The central guarantee under test is
// §"consistency": a batch `result` is byte-for-byte identical to the equivalent
// single-command `--json` output.

use std::io::Write;
use std::process::{Command, Stdio};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// Write a small but varied VCD: a clock, an 8-bit bus, and a valid/ready
/// handshake, with a handful of changes — enough to exercise every command.
fn write_vcd(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).expect("create tmp vcd");
    let body = "\
$timescale 1ns $end
$scope module tb $end
$var wire 1 ! clk $end
$var wire 8 # data $end
$var wire 1 $ valid $end
$var wire 1 % ready $end
$upscope $end
$enddefinitions $end
#0
0!
b00000000 #
0$
0%
#5
1!
#10
0!
b00010000 #
1$
#15
1!
1%
#20
0!
b00100000 #
0$
#25
1!
0%
";
    f.write_all(body.as_bytes()).expect("write vcd");
}

/// Run rwave with `args`, feeding `stdin_data`. Returns `(stdout, exit_code)`.
fn run(args: &[&str], stdin_data: &str) -> (String, i32) {
    let mut child = Command::new(rwave())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rwave");
    // A fatal or usage error makes rwave exit before it drains stdin (e.g.
    // `--batch info` bails immediately as a usage error, an unloadable file is
    // fatal). The BrokenPipe our write then sees is expected — we only need the
    // child's output and exit code — so don't treat it as a test failure. In
    // release builds the child exits fast enough that this race is reliable.
    let mut stdin = child.stdin.take().unwrap();
    match stdin.write_all(stdin_data.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin: {e:?}"),
    }
    drop(stdin);
    let out = child.wait_with_output().expect("wait");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Run a single-command `rwave --json <cmd...> <file>` (no stdin). Returns stdout.
fn single_json(file: &str, cmd: &[&str]) -> String {
    let mut args = vec!["--json"];
    args.extend_from_slice(cmd);
    args.push(file);
    let out = Command::new(rwave())
        .args(&args)
        .output()
        .expect("spawn rwave");
    assert!(
        out.status.success(),
        "single command failed: {cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn batch_result_is_byte_identical_to_single_json() {
    let vcd = std::env::temp_dir().join("rwave_batch_consistency.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // One representative invocation per command (covering all seven).
    let cmds: &[&[&str]] = &[
        &["info"],
        &["list", "--filter", "tb"],
        &["dump", "--begin", "0", "--end", "25ns", "--filter", "data"],
        &["summary", "--filter", "tb"],
        &["snapshot", "--at", "15ns"],
        &["compare", "--at", "10ns,20ns"],
        &["search", "--condition", "valid=1,ready=1", "--show", "data"],
    ];

    for cmd in cmds {
        let single = single_json(file, cmd);
        let single = single.trim_end_matches('\n');
        // The equivalent batch line is the command minus `rwave`/`--json`/file.
        let line = format!("{}\n", cmd.join(" "));
        let (batch_out, code) = run(&["--batch", "--json", file], &line);
        assert_eq!(code, 0, "batch exit for {cmd:?}");
        let batch_line = batch_out.trim_end_matches('\n');
        let expected = format!("{{\"id\":\"1\",\"ok\":true,\"result\":{single}}}");
        assert_eq!(batch_line, expected, "result mismatch for {cmd:?}");
    }

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_text_matches_single_text_per_block() {
    let vcd = std::env::temp_dir().join("rwave_batch_text.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // Single-command text output for `info`.
    let single = Command::new(rwave())
        .args(["info", file])
        .output()
        .expect("spawn");
    let single_text = String::from_utf8_lossy(&single.stdout).into_owned();

    // Batch text: a header line then the identical body.
    let (batch_out, code) = run(&["--batch", file], "info  #ov\n");
    assert_eq!(code, 0);
    let expected = format!("#ov\n{single_text}");
    assert_eq!(batch_out, expected);

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_error_isolation_keeps_running_exit_zero() {
    let vcd = std::env::temp_dir().join("rwave_batch_isolation.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    let input = "\
info
snapshot --at not_a_time
search --condition no_such_signal=1
list --filter tb
";
    let (out, code) = run(&["--batch", "--json", file], input);
    assert_eq!(code, 0, "batch with failing commands must still exit 0");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "one output line per command");
    assert!(lines[0].contains("\"ok\":true"), "info ok");
    assert!(
        lines[1].contains("\"ok\":false") && lines[1].contains("invalid time value"),
        "bad time isolated: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("\"ok\":false") && lines[2].contains("matches no signals"),
        "bad signal isolated: {}",
        lines[2]
    );
    assert!(lines[3].contains("\"ok\":true"), "list ok after failures");

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_empty_filter_match_is_ok_not_error() {
    // Iron rule: a command is ok:false iff the same single command exits
    // non-zero. An empty filter match is ok:true (exit 0 in single mode).
    let vcd = std::env::temp_dir().join("rwave_batch_emptyfilter.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    let (out, code) = run(
        &["--batch", "--json", file],
        "snapshot --at 9999us --filter does_not_exist\n",
    );
    assert_eq!(code, 0);
    let line = out.lines().next().unwrap();
    assert!(line.contains("\"ok\":true"), "empty filter is ok: {line}");

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_ids_labels_comments_and_sequence() {
    let vcd = std::env::temp_dir().join("rwave_batch_ids.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // Comments and blank lines are skipped and do not consume a sequence number;
    // labeled lines use their label, unlabeled lines use the running 1-based seq.
    let input = "\
# a comment, skipped

info
list --filter tb   #sigs
info
";
    let (out, code) = run(&["--batch", "--json", file], input);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "three real commands → three results");
    assert!(lines[0].starts_with("{\"id\":\"1\","), "first id is seq 1: {}", lines[0]);
    assert!(lines[1].starts_with("{\"id\":\"sigs\","), "second id is label: {}", lines[1]);
    assert!(lines[2].starts_with("{\"id\":\"3\","), "third id is seq 3: {}", lines[2]);

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_crlf_line_endings() {
    let vcd = std::env::temp_dir().join("rwave_batch_crlf.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    let (out, code) = run(&["--batch", "--json", file], "info\r\nlist --filter tb\r\n");
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 2);

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_malformed_line_is_isolated() {
    let vcd = std::env::temp_dir().join("rwave_batch_malformed.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // Unterminated quote → that line fails, batch continues, exit 0.
    let input = "info\nlist --filter \"oops\ninfo\n";
    let (out, code) = run(&["--batch", "--json", file], input);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[1].contains("\"ok\":false"), "malformed line failed: {}", lines[1]);

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_fatal_exit_codes() {
    let vcd = std::env::temp_dir().join("rwave_batch_fatal.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // --batch combined with a subcommand → usage error, exit 2.
    let (_o, code) = run(&["--batch", "info", file], "info\n");
    assert_eq!(code, 2, "batch + subcommand is a usage error");

    // Unloadable file → fatal, exit 1, no result lines.
    let (out, code) = run(&["--batch", "--json", "/no/such/file.vcd"], "info\n");
    assert_eq!(code, 1, "unloadable file is fatal");
    assert!(out.is_empty(), "no half output before a fatal load failure");

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn batch_global_defaults_apply_and_override() {
    let vcd = std::env::temp_dir().join("rwave_batch_defaults.vcd");
    write_vcd(&vcd);
    let file = vcd.to_str().unwrap();

    // A global --limit 1 caps every command; a per-line --limit 0 overrides it.
    let input = "dump --filter tb\ndump --filter tb --limit 0\n";
    let (out, code) = run(&["--batch", "--json", file, "--limit", "1"], input);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    // Equivalent single commands, built with the merged options, must match.
    let capped = single_json(file, &["dump", "--filter", "tb", "--limit", "1"]);
    let uncapped = single_json(file, &["dump", "--filter", "tb", "--limit", "0"]);
    let exp0 = format!("{{\"id\":\"1\",\"ok\":true,\"result\":{}}}", capped.trim_end());
    let exp1 = format!("{{\"id\":\"2\",\"ok\":true,\"result\":{}}}", uncapped.trim_end());
    assert_eq!(lines[0], exp0, "global --limit applied");
    assert_eq!(lines[1], exp1, "per-line --limit overrides global");

    let _ = std::fs::remove_file(&vcd);
}
