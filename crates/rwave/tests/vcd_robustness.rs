// Malformed-but-seen-in-the-wild VCD input: each used to abort, refuse the
// file, or corrupt `--json` stdout (vendored wellen patches, vendor/PATCHES.md).

use std::process::{Command, Output};

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

fn run_on(name: &str, body: &[u8], args: &[&str]) -> Output {
    let vcd = std::env::temp_dir().join(name);
    std::fs::write(&vcd, body).unwrap();
    let mut full: Vec<&str> = args.to_vec();
    let f = vcd.to_str().unwrap().to_string();
    full.insert(full.iter().position(|a| !a.starts_with("--")).unwrap() + 1, &f);
    let out = Command::new(rwave()).args(&full).output().expect("spawn rwave");
    let _ = std::fs::remove_file(&vcd);
    out
}

const HEAD: &str = "$timescale 1ns $end\n$scope module top $end\n\
    $var wire 4 ! v [3:0] $end\n$var wire 1 \" s $end\n$upscope $end\n$enddefinitions $end\n";

fn values(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .split("\"value\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().to_string())
        .collect()
}

#[test]
fn malformed_values_read_as_x_instead_of_panicking() {
    // A real written to logic ids, an invalid character, and a vector wider
    // than its declaration.
    let body = format!("{HEAD}#0\nb0 !\n0\"\n#10\nrnan !\nr1.5 \"\n#20\nb1 !\n0\"\n#30\nb11111 !\n#40\nbq1 !\n");
    let out = run_on("rwave_robust_values.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(values(&out.stdout), ["0x0", "0", "bxxxx", "bx", "0x1", "0", "bxxxx"]);
}

#[test]
fn utf8_bom_before_the_header_is_skipped() {
    let mut body = b"\xEF\xBB\xBF".to_vec();
    body.extend_from_slice(format!("{HEAD}#0\nb0 !\n#10\nb1 !\n").as_bytes());
    let out = run_on("rwave_robust_bom.vcd", &body, &["--json", "info"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"timescale\":\"1ns\""));
}

#[test]
fn backward_timestamp_warning_stays_off_stdout() {
    let body = format!("{HEAD}#0\n0\"\n#20\n1\"\n#10\n0\"\n#30\n0\"\n");
    let out = run_on("rwave_robust_backward.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success());
    assert!(out.stdout.starts_with(b"{\"command\":\"dump\""), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("time decreased"));
}

#[test]
fn missing_timescale_is_announced() {
    let body = "$scope module top $end\n$var wire 1 ! a $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n#10\n1!\n";
    let out = run_on("rwave_robust_nots.vcd", body.as_bytes(), &["info"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("declares no $timescale"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Timescale : (none declared; 1ps assumed)"));
}
