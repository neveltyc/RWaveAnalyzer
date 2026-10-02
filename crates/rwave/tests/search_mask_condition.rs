// Don't-care bits (`?`) in binary condition targets.
//
// Conditions compare whole values, so "bit 2 of status is 1" — 128 distinct
// values on an 8-bit bus — could not be asked at all. A `?` bit, as in a
// Verilog casez item, matches any value and leaves the other bits exact.
//
// The fixture follows the shape iverilog writes: a vector dump drops redundant
// leading zeros (`b10` for 8'h02) and compresses an x run (`b1xxxx` for
// 8'b0001_xxxx), so every comparison runs on the left-extended value.

use std::io::Write;
use std::process::{Command, Output};

fn write_vcd(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("rwave_mask_{name}_{}.vcd", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create tmp vcd");
    write!(
        f,
        "$timescale 1ns $end\n$scope module tb $end\n\
         $var reg 8 ! bus $end\n$var real 64 \" r $end\n$var wire 1 # bit $end\n\
         $upscope $end\n$enddefinitions $end\n\
         #0\nb0 !\nr0 \"\n0#\n\
         #10\nb10 !\n\
         #20\nb1010 !\n1#\n\
         #40\nb1xxxx !\n\
         #50\nb10000100 !\n\
         #60\n0#\n"
    )
    .unwrap();
    path
}

fn run(vcd: &std::path::Path, cond: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rwave"))
        .args(["search", vcd.to_str().unwrap(), "--condition", cond, "--begin", "0", "--end", "60ns", "--json"])
        .output()
        .expect("spawn rwave")
}

/// `(begin_ticks, end_ticks)` of every interval row.
fn spans(vcd: &std::path::Path, cond: &str) -> Vec<(u64, u64)> {
    let out = run(vcd, cond);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{cond}: {}", String::from_utf8_lossy(&out.stderr));
    let rows = stdout.split("\"rows\":").nth(1).expect("rows key");
    let mut v = Vec::new();
    for row in rows.split("\"begin_ticks\":").skip(1) {
        let b: u64 = row.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap();
        let e = row.split("\"end_ticks\":").nth(1).unwrap();
        let e: u64 = e.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap();
        v.push((b, e));
    }
    v
}

#[test]
fn mask_search_end_to_end() {
    let vcd = write_vcd("e2e");
    // bit 1 set in 0x02 and 0x0a.
    assert_eq!(spans(&vcd, "bus=b??????1?"), [(10, 40)]);
    assert_eq!(spans(&vcd, "bus=0b??????1?"), [(10, 40)]);
    // bit 4 is a known 1 beside the x run at 40ns.
    assert_eq!(spans(&vcd, "bus=b???1????"), [(40, 50)]);
    // bit 3 is x at 40ns, so not a 1 there.
    assert_eq!(spans(&vcd, "bus=b????1???"), [(20, 40)]);
    // top bit 1 and bit 2 set: only 0x84.
    assert_eq!(spans(&vcd, "bus=b1????1??"), [(50, 60)]);
    // A short mask pads with 0.
    assert_eq!(spans(&vcd, "bus=b1???"), [(20, 40)]);
    assert_eq!(spans(&vcd, "bus=b????????"), [(0, 60)]);
    assert_eq!(spans(&vcd, "bit=b?"), [(0, 60)]);
    // `!=`: an x under a `?` does not block; an x in a cared bit does.
    assert_eq!(spans(&vcd, "bus!=b???1????"), [(0, 40), (50, 60)]);
    assert_eq!(spans(&vcd, "bus!=b????1???"), [(0, 20), (50, 60)]);
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn mask_errors() {
    let vcd = write_vcd("err");
    let out = run(&vcd, "r=b1?");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("needs a logic signal"), "{err}");
    let out = run(&vcd, "bus=1??0");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("needs a binary prefix"), "{err}");
    let _ = std::fs::remove_file(&vcd);
}

/// Like `spans`, but with each element of `conds` as its own `--condition`.
fn or_spans(vcd: &std::path::Path, conds: &[&str]) -> Vec<(u64, u64)> {
    let mut args = vec!["search".to_string(), vcd.to_str().unwrap().to_string()];
    for c in conds {
        args.push("--condition".into());
        args.push(c.to_string());
    }
    args.extend(["--begin", "0", "--end", "60ns", "--json"].map(String::from));
    let out = Command::new(env!("CARGO_BIN_EXE_rwave")).args(&args).output().expect("spawn rwave");
    assert!(out.status.success(), "{conds:?}: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = stdout.split("\"rows\":").nth(1).expect("rows key");
    rows.split("\"begin_ticks\":")
        .skip(1)
        .map(|row| {
            let num = |s: &str| s.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap();
            (num(row), num(row.split("\"end_ticks\":").nth(1).unwrap()))
        })
        .collect()
}

#[test]
fn binary_and_decimal_targets_with_the_same_digits_do_not_fold() {
    // bus is 2 (b10) in [10,20) and 10 in [20,40). The binary body `10` used
    // to share a de-dup key with the decimal `10`, so one clause or term was
    // silently dropped and the answer depended on the order written.
    let vcd = write_vcd("dedup");
    assert_eq!(or_spans(&vcd, &["bus=b10", "bus=10"]), [(10, 40)]);
    assert_eq!(or_spans(&vcd, &["bus=10", "bus=b10"]), [(10, 40)]);
    // As an AND clause the two can never hold together.
    assert_eq!(spans(&vcd, "bus=b10,bus=10"), []);
    assert_eq!(spans(&vcd, "bus=10,bus=b10"), []);
    let _ = std::fs::remove_file(&vcd);
}
