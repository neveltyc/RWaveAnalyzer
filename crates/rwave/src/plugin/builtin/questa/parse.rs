// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Turning what `vsim` printed into [`Hop`]s, and its refusals into messages.
//!
//! Two output shapes, both captured verbatim from Questa 10.7c:
//!
//! ```text
//! find drivers -possible -tcl /top/res
//!   {Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4}
//!   {TRI /top <???> top.sv:13} {TRI /top <???> top.sv:14}   <- several, one line
//!
//! readers /top/u_core/u_alu/res
//!   Readers for trace:/top/u_core/u_alu/res:
//!      8'h55  : Net /top/res
//!          : Reader /top/#ALWAYS#9
//! ```
//!
//! Rows are validated structurally rather than by counting fields, because
//! several of Questa's refusals are bare prose with no marker on them
//! (`invalid command name "…"`). A line that does not have a Questa path where
//! a scope belongs is not a row, which keeps prose out of the results without
//! having to enumerate every sentence Questa might print.

use crate::backend::design::{Hop, HopKind, TraceStatus};

use super::tcl::split_list;

/// One row of `find drivers -possible`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Present only in the time-aware shape (`find drivers -time T`), which is
    /// parsed but not yet issued.
    pub time: Option<String>,
    /// Questa's own word: `Gate`, `Mux`, `FF`, `TRI`, `PROCESS`, ...
    pub kind: String,
    /// Questa path of the scope holding the driver.
    pub scope: String,
    /// The driving signal. `None` where Questa printed `<???>`, which it does
    /// for a driver it cannot name — a tristate branch, or a primitive with no
    /// net of its own.
    pub signal: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// rwave's dotted path, given the scope it was resolved in, as a Questa path.
///
/// The scope is passed in rather than re-derived by splitting `path`, because a
/// Verilog escaped identifier may itself contain a dot and splitting would cut
/// it in the wrong place.
pub fn to_questa(path: &str, scope: &str) -> String {
    let leaf = match path.strip_prefix(scope) {
        Some(rest) if !scope.is_empty() => rest.strip_prefix('.').unwrap_or(rest),
        _ => path,
    };
    if scope.is_empty() {
        return format!("/{leaf}");
    }
    format!("/{}/{}", scope.replace('.', "/"), leaf)
}

/// A Questa path as rwave spells it. The inverse of [`to_questa`] for every
/// name that does not itself contain a separator.
pub fn to_rwave(questa: &str) -> String {
    questa.trim_start_matches('/').replace('/', ".")
}

/// Split `dut.sv:4` into its parts, on the last `:` so a Windows drive letter
/// survives. A tail that is not a line number leaves the line unset rather than
/// discarding the file.
pub fn split_file_line(loc: &str) -> (Option<String>, Option<u32>) {
    let loc = loc.trim();
    if loc.is_empty() {
        return (None, None);
    }
    match loc.rsplit_once(':') {
        Some((f, n)) => match n.trim().parse::<u32>() {
            Ok(n) => (Some(f.to_string()), Some(n)),
            Err(_) => (Some(loc.to_string()), None),
        },
        None => (Some(loc.to_string()), None),
    }
}

/// Questa's driver type as the kind reported to users.
///
/// The vocabulary is Questa's netlist-primitive taxonomy and is open-ended:
/// `Gate` and `Mux` both turn up for continuous assignments, and the full list
/// lives in a message database rather than anywhere readable. So the sequential
/// and structural-boundary types are named, and anything else is a structural
/// primitive — which is what every row of a "possible drivers" table is. The
/// exact word is always on the hop as `raw_kind`, so nothing is lost by the
/// folding.
///
/// Nothing here folds into `ContAssign`. It was tempting, since the tristate
/// pair in the probe design is literally `assign bus = en ? d : 1'bz`, but
/// Questa also reports `Gate` for the adder inside an `always_ff`, where
/// "assign" would be false.
pub fn kind_of(questa_type: &str) -> HopKind {
    match questa_type {
        "FF" | "Latch" | "PROCESS" => HopKind::Procedural,
        "TRI" => HopKind::Tristate,
        "PORT" | "Port" => HopKind::Port,
        _ => HopKind::Gate,
    }
}

fn row_from_fields(f: &[String]) -> Option<Row> {
    // The scope column is always a Questa path. Requiring it is what rejects
    // prose without having to recognise the prose.
    let (time, rest): (Option<String>, &[String]) = match f.len() {
        4 if f[1].starts_with('/') => (None, f),
        5 if f[2].starts_with('/') => (Some(f[0].clone()), &f[1..]),
        _ => return None,
    };
    let signal = match rest[2].as_str() {
        "<???>" | "" => None,
        s => Some(s.to_string()),
    };
    let (file, line) = split_file_line(&rest[3]);
    Some(Row { time, kind: rest[0].clone(), scope: rest[1].clone(), signal, file, line })
}

/// Rows from `-tcl` output. Several drivers arrive as braced elements of a
/// single line, so each line is split twice: into rows, then into fields.
pub fn parse_tcl_rows(data: &[String]) -> Vec<Row> {
    let mut out = Vec::new();
    for line in data {
        for cand in split_list(line) {
            if let Some(row) = row_from_fields(&split_list(&cand)) {
                out.push(row);
            }
        }
    }
    out
}

/// Endpoint paths from `drivers`/`readers` output — the `Driver …` / `Reader …`
/// lines, ignoring the `Net …` lines above them, which are the same net seen at
/// other points in the hierarchy rather than endpoints of their own.
pub fn parse_endpoints(data: &[String], tag: &str) -> Vec<String> {
    let needle = format!(": {tag} ");
    let bare = format!("{tag} ");
    let mut out = Vec::new();
    for line in data {
        let rest = match line.find(&needle) {
            Some(i) => &line[i + needle.len()..],
            None => match line.trim_start().strip_prefix(&bare) {
                Some(r) => r,
                None => continue,
            },
        };
        let p = rest.trim();
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
    out
}

/// The leading scope of a Questa path: `/top/u/#ALWAYS#9` -> `/top/u`.
fn scope_of(questa_path: &str) -> String {
    match questa_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => questa_path[..i].to_string(),
    }
}

/// Driver rows as hops.
///
/// `signals` carries the driving signal as a full rwave path, which is what
/// lets `--at` resolve its value against the open waveform. `statement` holds
/// the driver's own expression: Questa's database has no statement text, so the
/// caller may replace this with the source line when it can read the file.
pub fn rows_to_hops(rows: &[Row]) -> Vec<Hop> {
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let full = r.signal.as_ref().map(|s| {
                let bare = s.split('[').next().unwrap_or(s);
                to_rwave(&format!("{}/{}", r.scope, bare))
            });
            Hop {
                group: i + 1,
                kind: kind_of(&r.kind),
                raw_kind: r.kind.clone(),
                statement: r.signal.clone().unwrap_or_default(),
                scope: to_rwave(&r.scope),
                file: r.file.clone(),
                line: r.line,
                boundary: false,
                signals: full.into_iter().collect(),
            }
        })
        .collect()
}

/// The construct a Questa process name denotes. Questa names an unnamed process
/// after the construct that created it — `#ASSIGN#13`, `#ALWAYS#9` — so the tag
/// is a statement of what it is, not a guess.
///
/// The trailing number is the source line, cross-checked against
/// `find drivers -possible` on the same process in every sample. It is not used:
/// the file is not in the name, and rwave prints a location only as `file:line`,
/// so a line on its own would add nothing to the output while making a
/// convention look like a contract.
pub fn kind_of_process(name: &str) -> HopKind {
    let tag = name.split('#').nth(1).unwrap_or("");
    match tag {
        "ASSIGN" | "CONTASSIGN" => HopKind::ContAssign,
        "ALWAYS" | "INITIAL" | "PROCESS" | "FUNC" | "TASK" => HopKind::Procedural,
        _ => HopKind::Other,
    }
}

/// Reader endpoints as hops.
///
/// Questa names the reading process (`/top/#ALWAYS#9`) but, in a
/// post-simulation session, reports no source location for it at all:
/// `readers -source` answers `line: -1`, `describe` and `find blocks` do not see
/// the process, and `find loads` is "not yet available" in this release. Its own
/// manual says as much — after simulation these commands carry topology only. So
/// a load hop states the construct and where it lives, and admits to no
/// location rather than inventing one.
pub fn endpoints_to_hops(endpoints: &[String]) -> Vec<Hop> {
    endpoints
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let leaf = p.rsplit('/').next().unwrap_or(p).to_string();
            Hop {
                group: i + 1,
                kind: kind_of_process(&leaf),
                raw_kind: leaf,
                // The full path, not the bare process name: with no location to
                // print, the name alone is what the reader has to tell two loads
                // apart, and `#ALWAYS#3` occurs once per module.
                statement: to_rwave(p),
                scope: to_rwave(&scope_of(p)),
                file: None,
                line: None,
                boundary: false,
                signals: Vec::new(),
            }
        })
        .collect()
}

/// The verdict for a set of hops. Mirrors `npi_dump::classify`, which cannot be
/// reused directly because it filters on `Control` hops that Questa never
/// produces — its debug database exposes no gating dependencies.
pub fn classify(hops: &[Hop]) -> TraceStatus {
    if hops.is_empty() {
        return TraceStatus::NotFound;
    }
    if hops.iter().all(|h| h.kind == HopKind::Port) {
        return TraceStatus::BoundaryOnly;
    }
    TraceStatus::Resolved
}

/// A refusal recognised well enough to say what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// No debug database beside the waveform.
    NoDebugDb,
    /// A database exists but was built without connectivity.
    NoCausality,
    /// The Questa work library is not reachable from the working directory.
    LibraryUnreachable,
    /// The name is not in the debug database.
    SignalNotFound,
    /// A licence could not be checked out.
    License(String),
}

/// Recognise a refusal in the collected output.
///
/// Runs over the payload as well as the diagnostics: Questa prints most of
/// these as ordinary transcript text and returns success, so a return code
/// would miss them. Ordered most specific first.
pub fn classify_fault(lines: &[String]) -> Option<Fault> {
    let joined = lines.join("\n");
    if joined.contains("required debug information has not been generated")
        || joined.contains("Could not open the database")
    {
        return Some(Fault::NoDebugDb);
    }
    if joined.contains("vopt was run without -debugdb")
        || joined.contains("Causality tracing is unavailable")
        || joined.contains("Schematic viewing and causality tracing unavailable")
    {
        return Some(Fault::NoCausality);
    }
    if joined.contains("_dbcontainer") || joined.contains("querying the internal debug database")
    {
        return Some(Fault::LibraryUnreachable);
    }
    if joined.contains("Signal not found") {
        return Some(Fault::SignalNotFound);
    }
    for l in lines {
        let low = l.to_ascii_lowercase();
        if low.contains("license") || low.contains("licence") {
            return Some(Fault::License(l.trim().to_string()));
        }
    }
    None
}

/// What the user should do about a fault. `dbg` and `alt` are the two places
/// Questa looks for a debug database; their existence is checked here, when the
/// message is written, never as a gate on trying.
pub fn fault_message(f: &Fault, wlf: &str, dbg: &str, dbg_exists: bool, alt_exists: bool) -> String {
    match f {
        Fault::NoDebugDb => {
            let found = match (dbg_exists, alt_exists) {
                (false, false) => "neither exists".to_string(),
                (true, _) => format!("{dbg} exists but could not be opened"),
                (false, true) => "only vsim.dbg exists, and it is for another design".to_string(),
            };
            format!(
                "vsim found no usable Questa debug database for {wlf} ({found}). \
                 Generate one with:\n  \
                 vopt +acc <top> -o <opt> -debugdb\n  \
                 vsim -postsimdataflow -debugdb=<name>.dbg -wlf <name>.wlf <opt>"
            )
        }
        Fault::NoCausality => format!(
            "{dbg} carries no connectivity: vopt ran without -debugdb. Re-run vopt with \
             -debugdb and re-simulate — the database must come from the same elaboration \
             as {wlf}."
        ),
        Fault::LibraryUnreachable => format!(
            "the Questa work library is not reachable from {wlf}'s directory, which is \
             where rwave runs vsim. `find drivers` reads the per-module databases under \
             <lib>/_dbcontainer, so run rwave against a waveform sitting in its own \
             simulation directory, with modelsim.ini mapping the library it was built in."
        ),
        Fault::SignalNotFound => {
            "vsim does not have this signal in its debug database".to_string()
        }
        Fault::License(t) => format!(
            "vsim could not check out a Questa licence: {t}. A WLF trace holds the licence \
             for as long as the rwave process runs."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from Questa 10.7c, `find drivers -possible -tcl`, one driver.
    const ONE_DRIVER: &str = "{Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4}";
    /// Two tristate drivers of one net, both on a single line, neither nameable.
    const TWO_TRI: &str = "{TRI /top <???> top.sv:13} {TRI /top <???> top.sv:14}";
    /// A flop driver.
    const FF_DRIVER: &str = "{FF /top {res[7:0]} top.sv:9}";
    /// Mixed kinds on one line, including a debugdb-internal temporary.
    const MIXED: &str =
        "{FF /top/u_core/u_alu {dbgTemp0_2[7:0]} dut.sv:3} {Gate /top/u_core/u_alu <???> dut.sv:3}";

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_a_single_driver_row() {
        let rows = parse_tcl_rows(&lines(ONE_DRIVER));
        assert_eq!(
            rows,
            vec![Row {
                time: None,
                kind: "Gate".into(),
                scope: "/top/u_core/u_alu".into(),
                signal: Some("res_q[7:0]".into()),
                file: Some("dut.sv".into()),
                line: Some(4),
            }]
        );
    }

    #[test]
    fn several_drivers_on_one_line_are_separate_rows() {
        let rows = parse_tcl_rows(&lines(TWO_TRI));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.signal.is_none()), "<???> is not a name");
        assert_eq!(rows[1].line, Some(14));

        let mixed = parse_tcl_rows(&lines(MIXED));
        assert_eq!(mixed.len(), 2);
        assert_eq!(mixed[0].kind, "FF");
        assert_eq!(mixed[1].kind, "Gate");
    }

    #[test]
    fn the_time_aware_shape_parses_too() {
        // Not issued yet, but parsing it now keeps the follow-up to a query change.
        let rows = parse_tcl_rows(&lines("{{55 ns} FF /top {res[7:0]} top.sv:9}"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].time.as_deref(), Some("55 ns"));
        assert_eq!(rows[0].scope, "/top");
    }

    #[test]
    fn prose_refusals_are_not_mistaken_for_rows() {
        // Questa answers several failures with a bare sentence and no marker.
        for prose in [
            "The \"find loads\" command is not yet available",
            "invalid command name \"this_is_not_a_command\"",
            "unknown or ambiguous subcommand \"readers\": must be blocks, classes",
            "Specified net must be in namespace 'sim:'",
            "The trace may be incomplete for the following reason(s):",
            "",
        ] {
            assert!(parse_tcl_rows(&lines(prose)).is_empty(), "parsed as a row: {prose}");
        }
    }

    #[test]
    fn reader_endpoints_come_from_the_reader_lines_only() {
        // Verbatim `readers /top/clk` output: the Net lines are the same net
        // seen elsewhere in the hierarchy, not endpoints.
        let out = lines(
            "Readers for trace:/top/clk:\n      : Net /top/#implicit-wire#0\n   1'h0  : Net /top/clk\n   1'h0  : Net /top/u_core/clk\n       : Reader /top/#ALWAYS#3\n       : Reader /top/#ALWAYS#9\n       : Reader /top/u_core/u_alu/#ALWAYS#3\n",
        );
        let eps = parse_endpoints(&out, "Reader");
        assert_eq!(
            eps,
            vec!["/top/#ALWAYS#3", "/top/#ALWAYS#9", "/top/u_core/u_alu/#ALWAYS#3"]
        );
        assert_eq!(parse_endpoints(&out, "Driver"), Vec::<String>::new());
    }

    #[test]
    fn driver_endpoints_use_the_same_shape() {
        let out = lines(
            "Drivers for /top/res:\n   8'h55  : Net /top/res\n       : Driver /top/u_core/u_alu/#ASSIGN#4\n",
        );
        assert_eq!(parse_endpoints(&out, "Driver"), vec!["/top/u_core/u_alu/#ASSIGN#4"]);
    }

    #[test]
    fn a_driver_hop_carries_an_rwave_path_so_at_can_resolve_it() {
        let hops = rows_to_hops(&parse_tcl_rows(&lines(ONE_DRIVER)));
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].kind, HopKind::Gate);
        assert_eq!(hops[0].raw_kind, "Gate");
        assert_eq!(hops[0].scope, "top.u_core.u_alu");
        // The bit range is dropped: rwave carries the whole vector under one name.
        assert_eq!(hops[0].signals, vec!["top.u_core.u_alu.res_q"]);
    }

    #[test]
    fn an_unnameable_driver_yields_no_endpoint_rather_than_a_guess() {
        let hops = rows_to_hops(&parse_tcl_rows(&lines(TWO_TRI)));
        assert_eq!(hops.len(), 2);
        assert!(hops.iter().all(|h| h.signals.is_empty()));
        assert_eq!(hops[0].kind, HopKind::Tristate);
    }

    #[test]
    fn reader_hops_state_the_process_and_admit_to_no_location() {
        let hops = endpoints_to_hops(&[
            "/top/u_core/u_alu/#ALWAYS#3".to_string(),
            "/top/#ASSIGN#13".to_string(),
        ]);
        // The full path: with no location to print, this is all the reader has
        // to tell two loads apart, and `#ALWAYS#3` recurs in every module.
        assert_eq!(hops[0].statement, "top.u_core.u_alu.#ALWAYS#3");
        assert_eq!(hops[0].raw_kind, "#ALWAYS#3");
        assert_eq!(hops[0].scope, "top.u_core.u_alu");
        // The name says what the construct is, so the kind column is not blank
        // even though Questa withholds the location after simulation.
        assert_eq!(hops[0].kind, HopKind::Procedural);
        assert_eq!(hops[1].kind, HopKind::ContAssign);
        assert!(hops.iter().all(|h| h.file.is_none() && h.line.is_none()));
    }

    #[test]
    fn an_unrecognised_process_tag_stays_unclassified() {
        assert_eq!(kind_of_process("#WHATEVER#7"), HopKind::Other);
        assert_eq!(kind_of_process("plain_name"), HopKind::Other);
    }

    #[test]
    fn paths_round_trip_through_questas_spelling() {
        assert_eq!(to_questa("top.u_core.res", "top.u_core"), "/top/u_core/res");
        assert_eq!(to_questa("top", ""), "/top");
        assert_eq!(to_rwave("/top/u_core/res"), "top.u_core.res");
        // The scope is trusted over splitting, so a dot inside a leaf survives.
        assert_eq!(to_questa("tb.\\foo.bar", "tb"), "/tb/\\foo.bar");
    }

    #[test]
    fn locations_split_on_the_last_colon() {
        assert_eq!(split_file_line("dut.sv:4"), (Some("dut.sv".into()), Some(4)));
        assert_eq!(
            split_file_line("C:\\p\\dut.sv:4"),
            (Some("C:\\p\\dut.sv".into()), Some(4))
        );
        assert_eq!(split_file_line("dut.sv"), (Some("dut.sv".into()), None));
        assert_eq!(split_file_line("dut.sv:x"), (Some("dut.sv:x".into()), None));
        assert_eq!(split_file_line("  "), (None, None));
    }

    #[test]
    fn kinds_map_without_claiming_more_than_questa_said() {
        assert_eq!(kind_of("FF"), HopKind::Procedural);
        assert_eq!(kind_of("PROCESS"), HopKind::Procedural);
        assert_eq!(kind_of("Latch"), HopKind::Procedural);
        assert_eq!(kind_of("TRI"), HopKind::Tristate);
        assert_eq!(kind_of("Gate"), HopKind::Gate);
        // `Mux` is what a ternary continuous assignment reports as. The
        // vocabulary is open-ended, so an unrecognised primitive is still a
        // structural driver rather than an unknown.
        assert_eq!(kind_of("Mux"), HopKind::Gate);
        assert_eq!(kind_of("SomeFuturePrimitive"), HopKind::Gate);
    }

    #[test]
    fn classifies_each_refusal_questa_actually_prints() {
        let cases = [
            (
                "Could not open the database because the required debug information has not been generated.",
                Fault::NoDebugDb,
            ),
            (
                "Causality tracing is unavailable because of the following problem accessing the required debug information:",
                Fault::NoCausality,
            ),
            (
                "error: \"vlib -libcmd exemptpath work/_dbcontainer/top_opt/work_top_fast_0.dbg\" failed!",
                Fault::LibraryUnreachable,
            ),
            ("Error: Signal not found (/top/no_such_xyz)", Fault::SignalNotFound),
        ];
        for (text, want) in cases {
            assert_eq!(classify_fault(&lines(text)).as_ref(), Some(&want), "{text}");
        }
        // A clean answer is not a fault.
        assert_eq!(classify_fault(&lines(ONE_DRIVER)), None);
        assert_eq!(classify_fault(&lines(FF_DRIVER)), None);
    }

    #[test]
    fn the_missing_database_message_names_the_commands_that_make_one() {
        let m = fault_message(&Fault::NoDebugDb, "sim.wlf", "sim.dbg", false, false);
        assert!(m.contains("vopt +acc"), "{m}");
        assert!(m.contains("-debugdb"), "{m}");
        assert!(m.contains("neither exists"), "{m}");
    }
}
