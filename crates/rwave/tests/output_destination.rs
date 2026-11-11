// `-o/--output FILE` and the exit-status contract: 0 answered (empty too),
// 1 the file or environment, 2 the request.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

const VCD: &str = "$timescale 1ns $end\n$scope module top $end\n\
$var wire 4 ! state [3:0] $end\n$var wire 1 \" clk $end\n$upscope $end\n$enddefinitions $end\n\
#0\nb11 !\n0\"\n#10\nb1001 !\n1\"\n#20\n0\"\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rwave_out_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("w.vcd"), VCD).unwrap();
    d
}

fn run(d: &Path, args: &[&str]) -> Output {
    Command::new(rwave()).current_dir(d).args(args).output().expect("spawn rwave")
}

fn leftovers(d: &Path) -> Vec<String> {
    std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect()
}

#[test]
fn output_file_holds_exactly_what_stdout_would() {
    let d = dir("same");
    for json in [false, true] {
        let mut args = vec!["dump", "w.vcd"];
        if json {
            args.insert(0, "--json");
        }
        let direct = run(&d, &args).stdout;
        let mut to_file = args.clone();
        to_file.extend(["-o", "r.txt"]);
        let o = run(&d, &to_file);
        assert!(o.status.success());
        assert_eq!(std::fs::read(d.join("r.txt")).unwrap(), direct);
        let receipt = String::from_utf8_lossy(&o.stdout);
        let lines = direct.iter().filter(|b| **b == b'\n').count();
        if json {
            assert!(receipt.starts_with("{\"command\":\"dump\",\"output\":"), "{receipt}");
            assert!(receipt.contains(&format!("\"bytes\":{},\"lines\":{lines}}}", direct.len())), "{receipt}");
        } else {
            assert!(receipt.starts_with(&format!("Wrote {lines} lines ({} bytes) to ", direct.len())), "{receipt}");
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_failed_run_publishes_nothing() {
    let d = dir("fail");
    let o = run(&d, &["search", "w.vcd", "--condition", "state=99", "--output", "r.txt"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    assert!(!d.join("r.txt").exists());
    assert!(leftovers(&d).is_empty(), "{:?}", leftovers(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn output_targets_that_cannot_be_right_are_refused() {
    let d = dir("refuse");
    let o = run(&d, &["dump", "w.vcd", "-o", "w.vcd"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(d.join("w.vcd")).unwrap(), VCD);
    let o = run(&d, &["dump", "w.vcd", "-o", "."]);
    assert_eq!(o.status.code(), Some(2));
    let o = run(&d, &["dump", "w.vcd", "-o", "no/such/dir/r.txt"]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(&d, &["dump", "w.vcd", "-o", ""]);
    assert_eq!(o.status.code(), Some(2));
    // A device is written in place, not replaced through a temp file.
    #[cfg(unix)]
    {
        let o = run(&d, &["dump", "w.vcd", "-o", "/dev/null"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        // A symlink target is written through, not replaced.
        std::os::unix::fs::symlink("real.txt", d.join("link.txt")).unwrap();
        assert!(run(&d, &["dump", "w.vcd", "-o", "link.txt"]).status.success());
        assert!(std::fs::symlink_metadata(d.join("link.txt")).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(d.join("real.txt")).unwrap().starts_with("Window:"));
        // A relative chain through another directory: written through, and
        // the receipt names the normalized real file.
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        std::os::unix::fs::symlink("../b/far.txt", d.join("a/lnk")).unwrap();
        let o = run(&d, &["dump", "w.vcd", "-o", "a/lnk"]);
        let receipt = String::from_utf8_lossy(&o.stdout);
        assert!(d.join("b/far.txt").exists() && receipt.trim_end().ends_with("/b/far.txt"), "{receipt}");
        // A loop is an error, and the links survive.
        std::os::unix::fs::symlink("loopB", d.join("loopA")).unwrap();
        std::os::unix::fs::symlink("loopA", d.join("loopB")).unwrap();
        assert_eq!(run(&d, &["dump", "w.vcd", "-o", "loopA"]).status.code(), Some(1));
        assert!(std::fs::symlink_metadata(d.join("loopA")).unwrap().file_type().is_symlink());
        // /dev/stdout with stdout redirected to a file writes into that file.
        let so = std::fs::File::create(d.join("so.txt")).unwrap();
        let o = Command::new(rwave()).current_dir(&d).args(["dump", "w.vcd", "-o", "/dev/stdout"])
            .stdout(so).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let body = std::fs::read_to_string(d.join("so.txt")).unwrap();
        assert!(body.starts_with("Window:") && body.contains("to /dev/stdout"), "{body}");
        // Appending to it keeps what was there (the descriptor is shared).
        std::fs::write(d.join("ap.txt"), "HEADER\n").unwrap();
        let ap = std::fs::OpenOptions::new().append(true).open(d.join("ap.txt")).unwrap();
        let o = Command::new(rwave()).current_dir(&d).args(["dump", "w.vcd", "-o", "/dev/stdout"])
            .stdout(ap).output().unwrap();
        assert!(o.status.success());
        let body = std::fs::read_to_string(d.join("ap.txt")).unwrap();
        assert!(body.starts_with("HEADER\nWindow:") && body.contains("to /dev/stdout"), "{body}");
    }
    // A failed run leaves an existing target as it was.
    std::fs::write(d.join("keep.txt"), "KEEP").unwrap();
    let o = run(&d, &["search", "w.vcd", "--condition", "state=99", "-o", "keep.txt"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "KEEP");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn batch_output_covers_the_run_not_a_line() {
    let d = dir("batch");
    let mut child = Command::new(rwave())
        .current_dir(&d)
        .args(["--batch", "w.vcd", "-o", "b.txt"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"info\ndump -o x.txt\n").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("Wrote "));
    let body = std::fs::read_to_string(d.join("b.txt")).unwrap();
    assert!(body.contains("#1\n") && body.contains("Error: --output applies to the whole batch run"), "{body}");
    assert!(!d.join("x.txt").exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn exit_status_says_who_can_fix_it() {
    let d = dir("exit");
    let code = |args: &[&str]| run(&d, args).status.code();
    assert_eq!(code(&["search", "w.vcd", "--condition", "state=15"]), Some(0)); // empty answer
    assert_eq!(code(&["search", "w.vcd", "--condition", "state=99"]), Some(2)); // cannot hold
    assert_eq!(code(&["search", "w.vcd", "--condition", "nope=1"]), Some(2)); // no such name
    assert_eq!(code(&["snapshot", "w.vcd", "--at", "banana"]), Some(2));
    assert_eq!(code(&["dump", "w.vcd", "--bogus"]), Some(2)); // usage
    assert_eq!(code(&["info", "missing.vcd"]), Some(1)); // the file
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn batch_error_lines_carry_their_exit_status() {
    let d = dir("batchexit");
    let mut child = Command::new(rwave())
        .current_dir(&d)
        .args(["--batch", "--json", "w.vcd"])
        .env("RWAVE_TRACE_EN", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"search --condition state=99\nbogus\ninfo\ntrace clk\n").unwrap();
    let o = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("\"ok\":false") && lines[0].ends_with("\"exit\":2}"), "{out}");
    assert!(lines[1].ends_with("\"exit\":2}"), "{out}");
    assert!(lines[2].contains("\"ok\":true"), "{out}");
    // trace switched off is the environment's failure.
    assert!(lines[3].ends_with("\"exit\":1}"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}
