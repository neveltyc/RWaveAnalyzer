// `summary`'s change count and `search changed()` answer one question: a
// signal's first value inside the window defines it (`defined_at`) and is not
// a change; an event counts every trigger.

use std::process::Command;

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

const VCD: &str = "$timescale 1ns $end\n$scope module top $end\n\
$var wire 1 ! a $end\n$var wire 4 \" late [3:0] $end\n$var event 1 # ev $end\n\
$upscope $end\n$enddefinitions $end\n\
#0\n0!\n#5\n1#\n#10\n1!\n#15\nb11 \"\n1#\n#20\n0!\nb101 \"\n#25\n1#\n";

fn out(args: &[&str]) -> String {
    let o = Command::new(rwave()).args(args).output().expect("spawn rwave");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn int_after(json: &str, key: &str) -> i64 {
    let at = json.find(key).unwrap_or_else(|| panic!("{key} in {json}")) + key.len();
    json[at..].split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

#[test]
fn summary_changes_agree_with_changed_events() {
    let vcd = std::env::temp_dir().join("rwave_summary_consistency.vcd");
    std::fs::write(&vcd, VCD).unwrap();
    let f = vcd.to_str().unwrap();
    for begin in ["0", "3ns", "12ns", "15ns", "16ns"] {
        for (sig, path) in [("a", "top.a"), ("late", "top.late[3:0]"), ("ev", "top.ev")] {
            let sum = out(&["--json", "summary", f, "--begin", begin, "--filter", sig]);
            let changes = if sum.contains("\"changes\":") { int_after(&sum, "\"changes\":") } else { 0 };
            let cond = format!("changed({path})");
            let found = out(&["--json", "search", f, "--begin", begin, "--condition", &cond]);
            let events = int_after(&found, "\"total\":");
            assert_eq!(changes, events, "{sig} from {begin}: summary {sum}\nsearch {found}");
        }
    }
    let sum = out(&["--json", "summary", f, "--begin", "12ns", "--filter", "late"]);
    assert!(sum.contains("\"defined_at_ticks\":15"), "{sum}");
    assert!(sum.contains("\"first_at_ticks\":20"), "{sum}");
    let _ = std::fs::remove_file(&vcd);
}
