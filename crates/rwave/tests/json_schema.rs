// The `--json` contract: shape follows the invocation, never the data.
//
// For a given command and a given set of flags the key set is fixed. That is
// the whole promise — a caller reads a key's value and never tests whether it
// is there — so it is checked directly here: each command is run against two
// data situations that used to produce different key sets, and the sets must
// come back identical. See docs/JSON.md.

use std::io::Write;
use std::process::Command;
use std::sync::OnceLock;

fn rwave() -> &'static str {
    env!("CARGO_BIN_EXE_rwave")
}

/// `moves` toggles, `still` is dumped once and then holds, `absent` is declared
/// and never written. Between them they cover the row states that used to add
/// and drop keys: active vs static, known vs undefined, unknown-free vs not.
const VCD: &str = "\
$timescale 1ns $end
$scope module tb $end
$var wire 1 ! moves $end
$var wire 4 \" still $end
$var wire 4 # absent $end
$var wire 1 $ late $end
$upscope $end
$enddefinitions $end
#0
0!
b0 \"
#10
1!
#20
0!
x$
#30
1!
";

/// Written exactly once per test binary. Every test here reads the same path
/// and they run as concurrent threads, so writing it per call would let one
/// test read the file while another was still filling it.
fn fixture() -> &'static std::path::Path {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rwave_schema_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let tmp = dir.join("schema.vcd.part");
        let mut f = std::fs::File::create(&tmp).expect("create tmp vcd");
        f.write_all(VCD.as_bytes()).expect("write vcd");
        let p = dir.join("schema.vcd");
        std::fs::rename(&tmp, &p).expect("publish fixture");
        p
    })
}

fn json(args: &[&str]) -> serde_lite::Value {
    let vcd = fixture();
    let mut argv: Vec<String> = vec![args[0].to_string(), vcd.display().to_string()];
    argv.extend(args[1..].iter().map(|s| s.to_string()));
    argv.push("--json".to_string());
    let out = Command::new(rwave()).args(&argv).output().expect("spawn rwave");
    assert!(
        out.status.success(),
        "rwave {argv:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_lite::parse(&String::from_utf8(out.stdout).expect("utf-8"))
}

/// Run with `--json` and return `(stdout, stderr, exit code)` without requiring
/// success.
fn raw(args: &[&str]) -> (String, String, i32) {
    let vcd = fixture();
    let mut argv: Vec<String> = vec![args[0].to_string(), vcd.display().to_string()];
    argv.extend(args[1..].iter().map(|s| s.to_string()));
    argv.push("--json".to_string());
    let out = Command::new(rwave()).args(&argv).output().expect("spawn rwave");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// A minimal JSON reader: the crate emits its own JSON and has no parser, and a
/// dependency for a test would ship in the binary's licence list.
mod serde_lite {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    impl Value {
        pub fn keys(&self) -> Vec<String> {
            match self {
                Value::Obj(m) => m.iter().map(|(k, _)| k.clone()).collect(),
                _ => panic!("not an object: {self:?}"),
            }
        }
        pub fn get(&self, k: &str) -> &Value {
            match self {
                Value::Obj(m) => m
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| panic!("missing key {k}: {self:?}")),
                _ => panic!("not an object: {self:?}"),
            }
        }
        pub fn arr(&self) -> &[Value] {
            match self {
                Value::Arr(v) => v,
                _ => panic!("not an array: {self:?}"),
            }
        }
    }

    pub fn parse(s: &str) -> Value {
        let b: Vec<char> = s.trim().chars().collect();
        let mut i = 0;
        value(&b, &mut i)
    }

    fn ws(b: &[char], i: &mut usize) {
        while *i < b.len() && b[*i].is_whitespace() {
            *i += 1;
        }
    }

    fn value(b: &[char], i: &mut usize) -> Value {
        ws(b, i);
        match b[*i] {
            '{' => {
                *i += 1;
                let mut m = Vec::new();
                ws(b, i);
                if b[*i] == '}' {
                    *i += 1;
                    return Value::Obj(m);
                }
                loop {
                    ws(b, i);
                    let k = match value(b, i) {
                        Value::Str(s) => s,
                        other => panic!("object key is not a string: {other:?}"),
                    };
                    ws(b, i);
                    assert_eq!(b[*i], ':');
                    *i += 1;
                    m.push((k, value(b, i)));
                    ws(b, i);
                    match b[*i] {
                        ',' => *i += 1,
                        '}' => {
                            *i += 1;
                            return Value::Obj(m);
                        }
                        c => panic!("unexpected {c} in object"),
                    }
                }
            }
            '[' => {
                *i += 1;
                let mut a = Vec::new();
                ws(b, i);
                if b[*i] == ']' {
                    *i += 1;
                    return Value::Arr(a);
                }
                loop {
                    a.push(value(b, i));
                    ws(b, i);
                    match b[*i] {
                        ',' => *i += 1,
                        ']' => {
                            *i += 1;
                            return Value::Arr(a);
                        }
                        c => panic!("unexpected {c} in array"),
                    }
                }
            }
            '"' => {
                *i += 1;
                let mut s = String::new();
                while b[*i] != '"' {
                    if b[*i] == '\\' {
                        *i += 1;
                        s.push(match b[*i] {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            c => c,
                        });
                    } else {
                        s.push(b[*i]);
                    }
                    *i += 1;
                }
                *i += 1;
                Value::Str(s)
            }
            't' => {
                *i += 4;
                Value::Bool(true)
            }
            'f' => {
                *i += 5;
                Value::Bool(false)
            }
            'n' => {
                *i += 4;
                Value::Null
            }
            _ => {
                let start = *i;
                while *i < b.len() && !matches!(b[*i], ',' | '}' | ']' | ' ') {
                    *i += 1;
                }
                let t: String = b[start..*i].iter().collect();
                Value::Num(t.parse().unwrap_or_else(|_| panic!("bad number {t}")))
            }
        }
    }
}

use serde_lite::Value;

/// The two runs must agree on keys, top level and row level alike.
fn same_shape(label: &str, a: &Value, b: &Value, rows_key: &str) {
    assert_eq!(a.keys(), b.keys(), "{label}: top-level keys differ");
    let (ra, rb) = (a.get(rows_key).arr(), b.get(rows_key).arr());
    // Both payloads must be non-empty. A comparison that silently passes for
    // want of rows would report a regression that emptied a result as
    // conforming.
    assert!(
        !ra.is_empty() && !rb.is_empty(),
        "{label}: nothing to compare ({} vs {} rows), the case is vacuous",
        ra.len(),
        rb.len()
    );
    assert_eq!(ra[0].keys(), rb[0].keys(), "{label}: row keys differ");
}

/// Every row of one result must have the same keys as every other. This is the
/// case the old output failed hardest: an active summary row carried timestamps
/// and a static one did not.
fn rows_uniform(label: &str, v: &Value, rows_key: &str) {
    let rows = v.get(rows_key).arr();
    // One row is trivially uniform with itself, so the case has to be fed at
    // least two to mean anything.
    assert!(rows.len() >= 2, "{label}: {} row(s), the case is vacuous", rows.len());
    let want = rows[0].keys();
    for (n, r) in rows.iter().enumerate() {
        assert_eq!(r.keys(), want, "{label}: row {n} has different keys");
    }
}

#[test]
fn command_is_stamped_on_every_result() {
    for (args, name) in [
        (vec!["info"], "info"),
        (vec!["list"], "list"),
        (vec!["dump"], "dump"),
        (vec!["summary"], "summary"),
        (vec!["snapshot", "--at", "10ns"], "snapshot"),
        (vec!["compare", "--at", "0,20ns"], "compare"),
        (vec!["search", "--condition", "moves=1"], "search"),
        (vec!["tree"], "tree"),
    ] {
        assert_eq!(json(&args).get("command"), &Value::Str(name.to_string()));
    }
}

/// Rows differ in what they hold, never in which keys they have.
#[test]
fn summary_rows_have_one_key_set() {
    // Active, static and unknown-carrying rows all in one result.
    let v = json(&["summary", "--verbose"]);
    rows_uniform("summary", &v, "rows");
    let kinds: Vec<&Value> = v.get("rows").arr().iter().map(|r| r.get("kind")).collect();
    assert!(kinds.contains(&&Value::Str("active".into())), "{kinds:?}");
    assert!(kinds.contains(&&Value::Str("static".into())), "{kinds:?}");
}

#[test]
fn snapshot_rows_have_one_key_set() {
    // At 0ns `late` and `absent` are undefined while `moves` is known, so both
    // row states are present in the same result.
    let v = json(&["snapshot", "--at", "0", "--verbose"]);
    rows_uniform("snapshot", &v, "signals");
    let undef: Vec<&Value> = v
        .get("signals")
        .arr()
        .iter()
        .map(|r| r.get("undefined"))
        .collect();
    assert!(undef.contains(&&Value::Bool(true)), "{undef:?}");
    assert!(undef.contains(&&Value::Bool(false)), "{undef:?}");
}

/// A selection option adds `matched`'s content, not the key itself.
#[test]
fn a_selection_does_not_change_the_key_set() {
    for (cmd, rows) in [
        (vec!["dump"], "events"),
        (vec!["summary"], "rows"),
        (vec!["snapshot", "--at", "10ns"], "signals"),
        // 0..10ns: `moves` is 0 at both 0 and 20ns, so a 0..20ns window would
        // leave the filtered run with no rows to compare.
        (vec!["compare", "--at", "0,10ns"], "diffs"),
    ] {
        let plain = json(&cmd);
        let mut filtered = cmd.clone();
        filtered.extend(["--filter", "moves"]);
        let sel = json(&filtered);
        same_shape(&format!("{cmd:?} +--filter"), &plain, &sel, rows);
        assert_eq!(plain.get("matched"), &Value::Null);
        assert_ne!(sel.get("matched"), &Value::Null);
    }
}

/// Truncation fills `hint`; it does not introduce it.
#[test]
fn truncation_does_not_change_the_key_set() {
    for (cmd, rows) in [
        (vec!["list"], "signals"),
        (vec!["dump"], "events"),
        (vec!["summary"], "rows"),
    ] {
        let full = json(&cmd);
        let mut clipped = cmd.clone();
        clipped.extend(["--limit", "1"]);
        let clipped = json(&clipped);
        same_shape(&format!("{cmd:?} +--limit"), &full, &clipped, rows);
        assert_eq!(full.get("hint"), &Value::Null);
        assert!(matches!(clipped.get("hint"), Value::Str(_)));
        assert_eq!(clipped.get("truncated"), &Value::Bool(true));
    }
}

/// An empty result keeps the shape and explains itself in `hint`.
#[test]
fn an_empty_result_keeps_the_key_set() {
    let full = json(&["dump"]);
    for extra in [
        vec!["--filter", "nosuchsignal"],
        vec!["--begin", "1ms"],
        vec!["--filter", "absent", "--exact"],
    ] {
        let mut cmd = vec!["dump"];
        cmd.extend(extra.iter().copied());
        let empty = json(&cmd);
        assert_eq!(full.keys(), empty.keys(), "{cmd:?}: keys differ when empty");
        assert_eq!(empty.get("shown"), &Value::Num(0.0), "{cmd:?}");
        assert!(matches!(empty.get("hint"), Value::Str(_)), "{cmd:?} has no hint");
    }
}

/// All three search modes answer under `rows`, with one row shape.
#[test]
fn search_modes_share_one_shape() {
    let interval = json(&["search", "--condition", "moves=1"]);
    let segment = json(&["search", "--condition", "moves=1", "--show", "still"]);
    let event = json(&["search", "--condition", "changed(moves)"]);

    assert_eq!(interval.keys(), segment.keys(), "interval vs segment");
    assert_eq!(interval.keys(), event.keys(), "interval vs event");
    for (label, v) in [("interval", &interval), ("segment", &segment), ("event", &event)] {
        rows_uniform(label, v, "rows");
    }
    assert_eq!(
        interval.get("rows").arr()[0].keys(),
        event.get("rows").arr()[0].keys(),
        "row keys differ across modes"
    );
    // An event is an instant, so it fills begin and leaves end null.
    assert_eq!(event.get("rows").arr()[0].get("end_ticks"), &Value::Null);
    assert_ne!(interval.get("rows").arr()[0].get("end_ticks"), &Value::Null);
    // `changed` exists in every mode; only its contents move.
    assert!(interval.get("changed").arr().is_empty());
    assert!(!event.get("changed").arr().is_empty());
}

/// `tree`'s two modes answer under one key too.
#[test]
fn tree_modes_share_one_shape() {
    let subtree = json(&["tree"]);
    let chain = json(&["tree", "--of", "tb.moves"]);
    assert_eq!(subtree.keys(), chain.keys());
    assert_eq!(subtree.get("signal"), &Value::Null);
    assert_ne!(chain.get("signal"), &Value::Null);
    assert_eq!(chain.get("depth"), &Value::Null);
}

/// Each time is written once, as a `_ticks`/`_h` pair. The bare aliases were
/// duplicates whose type did not even agree across commands.
#[test]
fn times_are_ticks_and_human_only() {
    for (cmd, absent) in [
        (vec!["snapshot", "--at", "10ns"], vec!["at"]),
        (vec!["compare", "--at", "0,20ns"], vec!["t1", "t2"]),
        (vec!["info"], vec!["time_min", "time_max", "duration"]),
    ] {
        let v = json(&cmd);
        let keys = v.keys();
        for k in absent {
            assert!(!keys.contains(&k.to_string()), "{cmd:?} still carries `{k}`: {keys:?}");
            // Both halves of the pair that replaced it must be there.
            for suffix in ["_ticks", "_h"] {
                let want = format!("{k}{suffix}");
                assert!(keys.contains(&want), "{cmd:?} has no `{want}`: {keys:?}");
            }
        }
    }
    // dump's event rows lost the duplicate `time` beside `time_ticks`.
    let rows = json(&["dump"]);
    let row = &rows.get("events").arr()[0];
    assert!(!row.keys().contains(&"time".to_string()), "{:?}", row.keys());
    assert!(matches!(row.get("time_ticks"), Value::Num(_)));
    assert!(matches!(row.get("time_h"), Value::Str(_)));
}

/// A failure under `--json` is JSON, on stderr, with stdout left clean.
#[test]
fn errors_are_json_under_json() {
    // Runtime error: the command is known, so it is named.
    let (out, err, code) = raw(&["dump", "--begin", "banana"]);
    assert_eq!(code, 1);
    assert!(out.trim().is_empty(), "stdout should stay clean: {out}");
    let v = serde_lite::parse(&err);
    assert_eq!(v.get("command"), &Value::Str("dump".into()));
    assert_eq!(v.get("ok"), &Value::Bool(false));
    assert!(matches!(v.get("error"), Value::Str(_)));

    // Usage error: rejected before a command was settled on.
    let (out, err, code) = raw(&["list", "--begin", "1ns"]);
    assert_eq!(code, 2);
    assert!(out.trim().is_empty(), "stdout should stay clean: {out}");
    let v = serde_lite::parse(&err);
    assert_eq!(v.get("command"), &Value::Null);
    assert_eq!(v.get("ok"), &Value::Bool(false));

    // Without --json the message is the plain one-liner it always was.
    let vcd = fixture();
    let out = Command::new(rwave())
        .args(["dump", vcd.to_str().unwrap(), "--begin", "banana"])
        .output()
        .expect("spawn");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.starts_with("Error: "), "{err}");
    assert!(!err.contains('{'), "{err}");
}

/// Every command carries the same four counts, in the same order.
#[test]
fn count_fields_are_uniform() {
    for args in [
        vec!["list"],
        vec!["dump"],
        vec!["summary"],
        vec!["snapshot", "--at", "10ns"],
        vec!["compare", "--at", "0,20ns"],
        vec!["search", "--condition", "moves=1"],
        vec!["tree"],
    ] {
        let keys = json(&args).keys();
        let idx = |k: &str| {
            keys.iter()
                .position(|x| x == k)
                .unwrap_or_else(|| panic!("{args:?} has no `{k}`: {keys:?}"))
        };
        let (shown, trunc, total, exact) =
            (idx("shown"), idx("truncated"), idx("total"), idx("total_is_exact"));
        assert!(
            shown + 1 == trunc && trunc + 1 == total && total + 1 == exact,
            "{args:?}: counts are not contiguous and in order: {keys:?}"
        );
        // `hint` follows them, and the payload array is last.
        assert!(idx("hint") == exact + 1, "{args:?}: hint does not follow the counts");
        assert_eq!(idx("hint"), keys.len() - 2, "{args:?}: payload is not last");
    }
}

/// `--verbose` adds a fixed set of fields and changes nothing else. The doc
/// sells this as the reason flag-dependent shape is acceptable, so it has to
/// hold: a caller knows whether it passed the flag.
#[test]
fn verbose_only_adds_fields() {
    for (cmd, rows) in [
        (vec!["list"], "signals"),
        (vec!["dump"], "events"),
        (vec!["summary"], "rows"),
        (vec!["snapshot", "--at", "10ns"], "signals"),
        (vec!["compare", "--at", "0,10ns"], "diffs"),
        (vec!["search", "--condition", "moves=1"], "rows"),
    ] {
        let plain = json(&cmd);
        let mut v = cmd.clone();
        v.push("--verbose");
        let verbose = json(&v);

        // Top level is untouched: --verbose is a row-level flag everywhere.
        assert_eq!(plain.keys(), verbose.keys(), "{cmd:?}: --verbose moved a top-level key");

        let (pr, vr) = (plain.get(rows).arr(), verbose.get(rows).arr());
        assert!(!pr.is_empty() && !vr.is_empty(), "{cmd:?}: no rows, the case is vacuous");
        let (pk, vk) = (pr[0].keys(), vr[0].keys());
        // The plain key set survives, in order, as a prefix of the verbose one.
        assert_eq!(pk, vk[..pk.len()], "{cmd:?}: --verbose reordered or dropped a key");
        assert!(vk.len() > pk.len(), "{cmd:?}: --verbose added nothing: {vk:?}");
    }
}

/// `total_is_exact` is false exactly where the command stops counting at the
/// limit, and true everywhere else — including on a truncated result from a
/// command that counted first.
#[test]
fn total_is_exact_tracks_who_counted() {
    // dump and search stop reading once the limit is met, so their totals are
    // lower bounds when clipped.
    for cmd in [vec!["dump"], vec!["search", "--condition", "moves=1"]] {
        let mut clipped = cmd.clone();
        clipped.extend(["--limit", "1"]);
        let v = json(&clipped);
        assert_eq!(v.get("truncated"), &Value::Bool(true), "{cmd:?}");
        assert_eq!(v.get("total_is_exact"), &Value::Bool(false), "{cmd:?}: total is a bound");
        assert_eq!(json(&cmd).get("total_is_exact"), &Value::Bool(true), "{cmd:?} complete");
    }
    // The rest build the whole set before clipping, so the total is exact even
    // when the rows are not all shown.
    for cmd in [vec!["list"], vec!["summary"], vec!["snapshot", "--at", "10ns"]] {
        let mut clipped = cmd.clone();
        clipped.extend(["--limit", "1"]);
        let v = json(&clipped);
        assert_eq!(v.get("truncated"), &Value::Bool(true), "{cmd:?}");
        assert_eq!(v.get("total_is_exact"), &Value::Bool(true), "{cmd:?}: total was counted");
    }
}

/// Every command's empty result explains itself. docs/JSON.md promises the
/// reason is in `hint`, and four commands used to return null while their text
/// renderer printed a sentence.
#[test]
fn every_empty_result_explains_itself() {
    for cmd in [
        vec!["list", "--filter", "nosuchsignal"],
        vec!["dump", "--filter", "nosuchsignal"],
        vec!["dump", "--begin", "1ms"],
        vec!["dump", "--filter", "absent", "--exact"],
        vec!["summary", "--filter", "nosuchsignal"],
        vec!["snapshot", "--at", "0", "--filter", "late", "--exact"],
        vec!["compare", "--at", "0,0"],
        vec!["search", "--condition", "moves=7"],
        vec!["search", "--condition", "changed(moves)", "--begin", "1ms", "--end", "2ms"],
    ] {
        let v = json(&cmd);
        assert_eq!(v.get("shown"), &Value::Num(0.0), "{cmd:?} is not empty");
        match v.get("hint") {
            Value::Str(h) => assert!(!h.is_empty(), "{cmd:?}: empty hint"),
            other => panic!("{cmd:?}: empty result with hint {other:?}"),
        }
    }
}

/// No message may carry the source indentation of a `\`-continued Rust string
/// literal, which arrives as a run of spaces mid-sentence.
#[test]
fn no_message_carries_source_indentation() {
    let probes: Vec<Vec<&str>> = vec![
        vec!["list", "--filter", "nosuchsignal"],
        vec!["list", "--limit", "1"],
        vec!["dump", "--filter", "nosuchsignal"],
        vec!["dump", "--filter", "absent", "--exact"],
        vec!["dump", "--begin", "1ms"],
        vec!["dump", "--filter", "moves,absent", "--begin", "5ns", "--end", "8ns"],
        vec!["dump", "--limit", "1"],
        vec!["summary", "--begin", "1ms"],
        vec!["summary", "--filter", "nosuchsignal"],
        vec!["snapshot", "--at", "0", "--filter", "late", "--exact"],
        vec!["compare", "--at", "0,0"],
        vec!["search", "--condition", "moves=7"],
    ];
    for cmd in probes {
        if let Value::Str(h) = json(&cmd).get("hint") {
            assert!(!h.contains("  "), "{cmd:?}: doubled space in hint: {h:?}");
        }
    }
    // The text renderers print the same sentences; check those too, since some
    // of them never reach a `hint` field.
    for cmd in [
        vec!["dump", "--filter", "absent", "--exact"],
        vec!["summary", "--begin", "1ms"],
        vec!["snapshot", "--at", "0", "--filter", "late", "--exact"],
        vec!["search", "--condition", "moves=7"],
    ] {
        let vcd = fixture();
        let mut argv: Vec<String> = vec![cmd[0].to_string(), vcd.display().to_string()];
        argv.extend(cmd[1..].iter().map(|s| s.to_string()));
        let out = Command::new(rwave()).args(&argv).output().expect("spawn");
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            // Table rows are space-padded on purpose; only prose is checked.
            if line.starts_with("  ") {
                continue;
            }
            assert!(!line.contains("  "), "{cmd:?}: doubled space in {line:?}");
        }
    }
}

/// Batch `result` is byte-identical to the equivalent single command, which is
/// what `commands/mod.rs` promises in order to have one implementation.
#[test]
fn batch_results_match_the_single_command() {
    let vcd = fixture();
    let cases: Vec<Vec<&str>> = vec![
        vec!["info"],
        vec!["list", "--filter", "moves"],
        vec!["dump", "--begin", "5ns"],
        vec!["summary", "--verbose"],
        vec!["snapshot", "--at", "10ns"],
        vec!["compare", "--at", "0,10ns"],
        vec!["search", "--condition", "moves=1"],
        vec!["tree"],
    ];
    let script: String = cases.iter().map(|c| format!("{}\n", c.join(" "))).collect();

    let out = Command::new(rwave())
        .args(["--batch", vcd.to_str().unwrap(), "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut ch| {
            use std::io::Write as _;
            ch.stdin.take().unwrap().write_all(script.as_bytes())?;
            ch.wait_with_output()
        })
        .expect("run batch");
    let batch = String::from_utf8(out.stdout).expect("utf-8");
    let lines: Vec<&str> = batch.lines().collect();
    assert_eq!(lines.len(), cases.len(), "batch returned {} lines", lines.len());

    for (line, cmd) in lines.iter().zip(&cases) {
        let framed = serde_lite::parse(line);
        assert_eq!(framed.get("ok"), &Value::Bool(true), "{cmd:?}: {line}");
        // Compare the serialized `result` against the single command's stdout,
        // byte for byte — re-serializing through the parser would hide an
        // ordering difference.
        let key = "\"result\":";
        let start = line.find(key).expect("no result member") + key.len();
        let single = json_raw(cmd);
        assert_eq!(
            &line[start..line.len() - 1],
            single.trim(),
            "{cmd:?}: batch result differs from the single command"
        );
    }
}

/// The single command's raw stdout, for the byte-identity comparison above.
fn json_raw(args: &[&str]) -> String {
    let vcd = fixture();
    let mut argv: Vec<String> = vec![args[0].to_string(), vcd.display().to_string()];
    argv.extend(args[1..].iter().map(|s| s.to_string()));
    argv.push("--json".to_string());
    let out = Command::new(rwave()).args(&argv).output().expect("spawn rwave");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8")
}
