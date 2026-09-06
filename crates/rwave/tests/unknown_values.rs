// The value-format contract for unknown bits, end to end: every value with an
// x/z bit — 1-bit included — carries the `b` prefix, no clean value does, and
// `summary` flags the signals that carried one.
//
// The motivating failure: 1-bit unknowns used to print bare `x`/`z` while
// buses printed `b01x0`, so the only common marker was the letter itself, and
// `'x' in value` matches every `0x..` hex value.

use std::io::Write;
use std::process::{Command, Output};
use std::sync::OnceLock;

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// `a`: 1-bit, x at 0, 1 at 10, z at 30.
/// `bus`: 4-bit, x at 0, 0b1011 at 10, clean from then on.
/// `clean`: 4-bit, 0b1011 at 0, 0b0011 at 20 — never unknown.
/// `late`: 1-bit, 0 at 0, x at 30 — clean baseline, unknown only later.
const VCD: &str = "\
$timescale 1ns $end
$scope module top $end
$var reg 1 ! a $end
$var reg 4 \" bus [3:0] $end
$var reg 4 # clean [3:0] $end
$var reg 1 $ late $end
$upscope $end
$enddefinitions $end
#0
x!
bxxxx \"
b1011 #
0$
#10
1!
b1011 \"
#20
b0011 #
#30
z!
x$
";

/// The fixture, written exactly once per test binary.
///
/// Every test here reads the same path, and they run concurrently as threads
/// of one binary. Rewriting it per call raced: `File::create` truncates before
/// `write_all` fills it, so another test's `rwave` could open the file inside
/// that window and be handed zero bytes — "unknown file format", intermittently
/// and only under CI's timing. `OnceLock` makes the write happen before any
/// reader has the path, and the rename means the path never names a partial
/// file even if a previous run left the directory behind.
fn vcd() -> &'static std::path::Path {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rwave_unknown_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("unknown.vcd.part");
        std::fs::File::create(&tmp).unwrap().write_all(VCD.as_bytes()).unwrap();
        let path = dir.join("unknown.vcd");
        std::fs::rename(&tmp, &path).unwrap();
        path
    })
}

fn run(args: &[&str]) -> Output {
    let path = vcd();
    let mut cmd = Command::new(rwave());
    cmd.arg("--json").arg(args[0]).arg(&path).args(&args[1..]);
    cmd.output().unwrap()
}

fn stdout(args: &[&str]) -> String {
    let out = run(args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// Every `"value":"..."` string in the output.
fn values(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find("\"value\":\"") {
        rest = &rest[i + 9..];
        let end = rest.find('"').unwrap();
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

#[test]
fn one_bit_unknowns_carry_the_b_prefix() {
    let out = stdout(&["dump", "--begin", "0", "--end", "40ns", "--filter", "top.a"]);
    assert_eq!(values(&out), ["bx", "1", "bz"]);
}

#[test]
fn b_prefix_is_exactly_has_unknown_bits() {
    let out = stdout(&["dump", "--begin", "0", "--end", "40ns"]);
    let vals = values(&out);
    assert_eq!(vals.len(), 9, "{out}");
    for v in &vals {
        let has_xz = v.contains('x') && !v.starts_with("0x") || v.contains('z');
        assert_eq!(v.starts_with('b'), has_xz, "{v}");
    }
    // The hex prefix is why `'x' in value` is the wrong test.
    assert!(vals.iter().any(|v| v.starts_with("0x")), "{out}");
}

#[test]
fn snapshot_and_compare_use_the_same_rendering() {
    let snap = stdout(&["snapshot", "--at", "35ns"]);
    let mut vals = values(&snap);
    vals.sort();
    assert_eq!(vals, ["0x3", "0xb", "bx", "bz"]);

    let cmp = stdout(&["compare", "--at", "5ns,35ns", "--filter", "top.a,top.late"]);
    assert!(cmp.contains("\"at_t1\":\"bx\",\"at_t2\":\"bz\""), "{cmp}");
    assert!(cmp.contains("\"at_t1\":\"0\",\"at_t2\":\"bx\""), "{cmp}");
}

#[test]
fn output_values_round_trip_into_conditions() {
    // `bx` and `bz` parse as 4-state targets, so a value read from one command
    // can be pasted into the next.
    let out = stdout(&["search", "--condition", "top.a=bz", "--begin", "0", "--end", "40ns"]);
    assert!(out.contains("\"begin_ticks\":30,"), "{out}");
    let out = stdout(&["search", "--condition", "late=bx", "--begin", "0", "--end", "40ns"]);
    assert!(out.contains("\"begin_ticks\":30,"), "{out}");
}

#[test]
fn summary_flags_signals_that_carried_an_unknown() {
    let out = stdout(&["summary"]);
    assert!(out.contains("\"unknown\":3,"), "{out}");
    let row = |name: &str| {
        let start = out.find(&format!("\"path\":\"top.{name}")).unwrap_or_else(|| panic!("{name}"));
        let rest = &out[start..];
        rest[..rest.find('}').unwrap()].to_string()
    };
    for name in ["a", "bus", "late"] {
        assert!(row(name).contains("\"unknown\":true"), "{}", row(name));
    }
    // Every row carries the key; a clean one carries it as false. The flag has
    // to be read, not merely looked for.
    assert!(row("clean").contains("\"unknown\":false"), "{}", row("clean"));
}

#[test]
fn summary_unknown_follows_the_window() {
    // 15..25ns: `a` and `bus` are clean by then, `late` is not yet x.
    let out = stdout(&["summary", "--begin", "15ns", "--end", "25ns"]);
    assert!(out.contains("\"unknown\":0,"), "{out}");
    assert!(!out.contains("\"unknown\":true"), "{out}");
    // A signal stuck at x with no change in the window still counts.
    let out = stdout(&["summary", "--begin", "2ns", "--end", "5ns", "--filter", "bus"]);
    assert!(out.contains("\"static\":1,\"unknown\":1,"), "{out}");
    assert!(out.contains("\"value\":\"bxxxx\""), "{out}");
}
