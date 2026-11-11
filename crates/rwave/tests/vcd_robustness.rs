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
    let out = run_on("rwave_robust_nots.vcd", body.as_bytes(), &["--json", "info"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"timescale\":\"\",\"timescale_assumed\":true"));
}

#[test]
fn malformed_scalar_real_and_string_values() {
    let head = "$timescale 1ns $end\n$scope module top $end\n\
        $var wire 1 ! a $end\n$var wire 4 \" v [3:0] $end\n$var real 64 # r $end\n\
        $var string 1 $ s $end\n$upscope $end\n$enddefinitions $end\n";
    // a: r1 / b1x0 are not one bit; b01 is 1. v: an empty body is 0, extra
    // leading zeros carry no value, a 1 above the width is x. r: an
    // unparsable real is NaN. s: a value without the `s` prefix is kept.
    let body = format!(
        "{head}#0\n0!\nb \"\nr1.5 #\nshi $\n#1\nr1 !\nb00011 \"\nrfoo #\nb1 $\n\
         #2\n0!\nb10011 \"\n#3\nb1x0 !\n#4\nb01 !\n"
    );
    let out = run_on("rwave_robust_more.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        values(&out.stdout),
        ["0", "0x0", "1.5", "hi", "bx", "0x3", "NaN", "b1", "0", "bxxxx", "bx", "1"]
    );
}

#[test]
fn pymtl_prefix_uppercase_empty_and_undeclared_ids() {
    // pymtl3 writes `0b` on 1-bit values too; `B` alone is an empty body (0);
    // a change on an undeclared id (`1!!`, `0?`) is skipped with one warning.
    let head = "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! clk $end\n\
        $upscope $end\n$enddefinitions $end\n";
    let body = format!("{head}#0\nb0b0 !\n#1\nb0b1 !\n#2\nB !\n#3\n1!!\n0?\n#4\n1!\n");
    let out = run_on("rwave_robust_pymtl.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(values(&out.stdout), ["0", "1", "0", "1"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.matches("undeclared id").count(), 1, "{err}");
}

#[test]
fn stray_letters_on_a_scalar_read_as_x_and_sparse_ids_skip() {
    // `bb`/`bB`/`b0bb` are not a bit (x, as on a vector). A long id makes
    // wellen use its id map, whose miss on `1!!` must skip, not panic.
    let head = "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! a $end\n\
        $var wire 1 ~~~~~~ b $end\n$upscope $end\n$enddefinitions $end\n";
    let body = format!("{head}#0\n0!\n0~~~~~~\n#1\nbb !\n#2\n0!\n#3\nbB !\n#4\n0!\n#5\nb0bb !\n1!!\n#6\n1~~~~~~\n#7\n0!\n#8\nb0b !\n");
    let out = run_on("rwave_robust_sparse.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(values(&out.stdout), ["0", "0", "bx", "0", "bx", "0", "bx", "1", "0", "bx"]);
}

#[test]
fn every_nan_is_one_value() {
    // `rnan` and `r-nan` parse to different bit patterns; neither is a change
    // from the other (glibc prints x86's default NaN as `-nan`).
    let head = "$timescale 1ns $end\n$scope module t $end\n$var real 64 ! r $end\n\
        $upscope $end\n$enddefinitions $end\n";
    let body = format!("{head}#0\nr1.5 !\n#10\nrnan !\n#20\nr-nan !\n#30\nrnan !\n#40\nr2 !\n");
    let out = run_on("rwave_robust_nan.vcd", body.as_bytes(), &["--json", "dump"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(values(&out.stdout), ["1.5", "NaN", "2"]);
    let out = run_on("rwave_robust_nan.vcd", body.as_bytes(), &["--json", "summary"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"changes\":2"));
}
