// A bus dumped one bit per `$var` (Questa/ModelSim style) is reassembled by
// wellen into a derived signal. Its sid must keep wellen's derived flag: with
// the flag stripped, the bus read the ground signal of the same index (here
// `a`) or nothing at all, and could be grouped as an alias of it.

use std::process::Command;

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

fn write_vcd(path: &std::path::Path) {
    std::fs::write(
        path,
        "$timescale 1ns $end\n$scope module top $end\n\
         $var wire 1 # a $end\n\
         $var wire 1 ! d [0] $end\n$var wire 1 \" d [1] $end\n$var wire 1 $ d [2] $end\n\
         $upscope $end\n$enddefinitions $end\n\
         #0\n1#\n0!\n0\"\n0$\n#10\n1!\n1\"\n#20\n0#\n0!\n0\"\n1$\n",
    )
    .unwrap();
}

fn run(args: &[&str]) -> String {
    let out = Command::new(rwave()).args(args).output().expect("spawn rwave");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn bit_exploded_bus_keeps_its_data() {
    let vcd = std::env::temp_dir().join("rwave_bit_exploded_bus.vcd");
    write_vcd(&vcd);
    let f = vcd.to_str().unwrap();

    let out = run(&["--json", "dump", f, "--filter", "d[2:0]", "--exact"]);
    let vals: Vec<&str> = out
        .split("\"value\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    assert_eq!(vals, ["0x0", "0x3", "0x4"], "{out}");

    // The scalar `a` is its own signal, not an alias of the derived bus.
    let out = run(&["--json", "snapshot", f, "--at", "15ns"]);
    assert!(out.contains("\"path\":\"top.a\""), "{out}");
    assert!(out.contains("\"path\":\"top.d[2:0]\""), "{out}");
    assert!(!out.contains("aliases"), "{out}");

    let out = run(&["search", f, "--condition", "d[2:0]=3", "--exact"]);
    assert!(out.contains("10ns"), "{out}");

    let _ = std::fs::remove_file(&vcd);
}
