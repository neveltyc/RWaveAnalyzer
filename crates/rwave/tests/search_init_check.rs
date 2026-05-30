// Regression tests for the search interval/segment-mode init-check at
// end-of-stream (commands.rs).
//
// Two cases:
//   1. Condition holds throughout `[--begin, --end]` and there are no events
//      strictly past `--begin`. Must emit exactly one interval `[--begin,
//      --end)`. Pre-fix this silently emitted nothing (bug commit 6efd002).
//
//   2. Degenerate zero-width window `--begin T --end T`. Must emit nothing
//      even when the condition holds; the post-fix end-of-stream init-check
//      must NOT materialize a `[T, T)` row. Pre-fix-of-the-fix this was a
//      regression introduced by case 1's fix.

use std::io::Write;
use std::process::Command;

fn write_test_vcd(path: &std::path::Path) {
    // A single 1-bit signal `flag` that goes high at tick 0 and never changes.
    // Any --condition `flag=1` with --begin > 0 hits the "no events past t0"
    // path because the only event is at tick 0 (absorbed into baseline).
    let mut f = std::fs::File::create(path).expect("create tmp vcd");
    writeln!(f, "$timescale 1ns $end").unwrap();
    writeln!(f, "$scope module tb $end").unwrap();
    writeln!(f, "$var wire 1 ! flag $end").unwrap();
    writeln!(f, "$upscope $end").unwrap();
    writeln!(f, "$enddefinitions $end").unwrap();
    writeln!(f, "#0").unwrap();
    writeln!(f, "1!").unwrap();
}

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

#[test]
fn search_interval_no_events_past_begin_emits_one_row() {
    let vcd = std::env::temp_dir().join("rwave_init_check_ok.vcd");
    write_test_vcd(&vcd);
    let out = Command::new(rwave())
        .args([
            "search",
            vcd.to_str().unwrap(),
            "--condition",
            "flag=1",
            "--begin",
            "100ns",
            "--end",
            "500ns",
        ])
        .output()
        .expect("spawn rwave");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "rwave exited {}: stdout={stdout}, stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // Expect exactly one interval covering the requested window.
    assert!(
        stdout.contains("Found: 1") && stdout.contains("100ns") && stdout.contains("500ns"),
        "expected one interval [100ns,500ns); got:\n{stdout}"
    );
    let _ = std::fs::remove_file(&vcd);
}

#[test]
fn search_interval_zero_width_window_emits_nothing() {
    let vcd = std::env::temp_dir().join("rwave_init_check_zero.vcd");
    write_test_vcd(&vcd);
    let out = Command::new(rwave())
        .args([
            "search",
            vcd.to_str().unwrap(),
            "--condition",
            "flag=1",
            "--begin",
            "5ns",
            "--end",
            "5ns",
        ])
        .output()
        .expect("spawn rwave");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "rwave exited {}: stdout={stdout}, stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // Reference behavior: no interval for a zero-width window, even when the
    // condition holds at t0.
    assert!(
        stdout.contains("No interval"),
        "expected 'No interval', got:\n{stdout}"
    );
    let _ = std::fs::remove_file(&vcd);
}
