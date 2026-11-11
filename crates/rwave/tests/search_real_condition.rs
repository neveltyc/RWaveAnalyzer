// A condition signal's recorded value is classified as a logic-bit string by
// the signal's declared *kind*, never by sniffing its rendered characters.
//
// A real signal valued 100.0 renders as "100". The old char-sniffing heuristic
// treated that as the binary vector 100 (= 4), so `dac=4` spuriously matched
// and `dac=100` missed. A real signal compares a decimal or real number
// numerically; a bit pattern on it is an error.

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
fn real_signal_compares_numerically() {
    let vcd = std::env::temp_dir().join("rwave_cond_kind_real.vcd");
    write_real_vcd(&vcd);

    // dac is real (100.0 -> "100"). `dac=4` must NOT match: read as bits,
    // "100" would be binary 100 == 4.
    let out = search(&vcd, "dac=4");
    assert!(out.contains("No interval"), "dac=4 must not match; got:\n{out}");

    // `dac=100` and `dac=100.0` ask the same numeric question.
    for cond in ["dac=100", "dac=100.0", "dac=1e2"] {
        let out = search(&vcd, cond);
        assert!(out.contains("Found: 1 interval"), "{cond} must match; got:\n{out}");
    }
    let out = search(&vcd, "dac!=100");
    assert!(out.contains("No interval"), "{out}");

    // A bit pattern has no meaning on a real.
    for cond in ["dac=b100", "dac=0x64", "dac=1x"] {
        let out = Command::new(rwave())
            .args(["search", vcd.to_str().unwrap(), "--condition", cond])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{cond}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("against a bit pattern"), "{cond}");
    }

    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn logic_signal_still_matches() {
    // Positive control: a genuine logic signal matches as bits.
    let vcd = std::env::temp_dir().join("rwave_cond_kind_logic.vcd");
    write_real_vcd(&vcd);

    let out = search(&vcd, "clk=1");
    assert!(
        out.contains("Found") && out.contains("10ns") && out.contains("20ns"),
        "clk=1 should yield interval [10ns, 20ns); got:\n{out}"
    );

    let _ = std::fs::remove_file(&vcd);
}
