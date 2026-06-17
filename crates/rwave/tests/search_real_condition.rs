// Regression test for the condition-kind fix (commands/search.rs): a condition
// signal's recorded value is classified as a logic-bit string by the signal's
// declared *kind*, never by sniffing its rendered characters.
//
// A real signal valued 100.0 renders as "100". The old char-sniffing heuristic
// treated that as the binary vector 100 (= 4), so `dac=4` spuriously matched
// and `dac=100` missed. Reals/strings must now never satisfy a numeric/bit
// target; only genuine logic signals do.

use std::io::Write;
use std::process::Command;

/// A VCD with a real signal `dac` held at 100.0 and a 1-bit `clk` that is high
/// only in [10ns, 20ns).
fn write_real_vcd(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).expect("create tmp vcd");
    writeln!(f, "$timescale 1ns $end").unwrap();
    writeln!(f, "$scope module tb $end").unwrap();
    writeln!(f, "$var real 64 ! dac $end").unwrap();
    writeln!(f, "$var wire 1 \" clk $end").unwrap();
    writeln!(f, "$upscope $end").unwrap();
    writeln!(f, "$enddefinitions $end").unwrap();
    writeln!(f, "#0").unwrap();
    writeln!(f, "r100.0 !").unwrap();
    writeln!(f, "0\"").unwrap();
    writeln!(f, "#10").unwrap();
    writeln!(f, "1\"").unwrap();
    writeln!(f, "#20").unwrap();
    writeln!(f, "0\"").unwrap();
}

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

fn search(vcd: &std::path::Path, cond: &str) -> String {
    let out = Command::new(rwave())
        .args([
            "search",
            vcd.to_str().unwrap(),
            "--condition",
            cond,
            "--begin",
            "0",
            "--end",
            "20ns",
        ])
        .output()
        .expect("spawn rwave");
    assert!(
        out.status.success(),
        "rwave search {cond} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn real_signal_not_matched_as_binary() {
    let vcd = std::env::temp_dir().join("rwave_cond_kind_real.vcd");
    write_real_vcd(&vcd);

    // dac is real (100.0 -> "100"). `dac=4` must NOT match — pre-fix the "100"
    // text was parsed as binary 100 == 4, a false positive.
    let out = search(&vcd, "dac=4");
    assert!(
        out.contains("No interval"),
        "dac=4 must not match a real-valued signal; got:\n{out}"
    );

    // `dac=100` must also not match: reals are not numerically comparable here.
    let out = search(&vcd, "dac=100");
    assert!(
        out.contains("No interval"),
        "dac=100 must not match a real-valued signal; got:\n{out}"
    );

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn logic_signal_still_matches() {
    // Positive control: a genuine logic signal still matches normally, proving
    // the fix only suppresses matching on non-logic kinds.
    let vcd = std::env::temp_dir().join("rwave_cond_kind_logic.vcd");
    write_real_vcd(&vcd);

    let out = search(&vcd, "clk=1");
    assert!(
        out.contains("Found") && out.contains("10ns") && out.contains("20ns"),
        "clk=1 should yield interval [10ns, 20ns); got:\n{out}"
    );

    let _ = std::fs::remove_file(&vcd);
}
