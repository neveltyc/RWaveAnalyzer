// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Parser for the text Verdi's NPI L1 connectivity API writes to a `FILE*`.
//!
//! Deliberately kept out of the `fsdb` module, which is gated to linux-x86_64
//! because it dlopens vendor libraries. Nothing here touches FFI — it is a
//! string-to-[`Hop`] transform — so living outside that gate means its tests
//! run on every platform rather than only where NPI happens to be loadable.
//!
//! The grammar, as emitted by Verdi V-2023.12 for
//! `npi_trace_{driver,load}_dump2`:
//!
//! ```text
//! npiReg, tb.u_core.res, {/p/dut.sv : 15} /* results of trace driver */
//! Need pass through
//! <1> source: res, scope: tb.u_core
//!     pass through.                          <- optional group marker
//!     <D> npiContAssign, assign res = q, {/p/dut.sv : 27}
//!           npiReg, tb.u_core.q, {/p/dut.sv : 17}
//! ```
//!
//! `<N>` opens a group, `<D>`/`<L>` is the driving/loading statement, and the
//! deeper-indented lines under it are the signals that statement touches.

use crate::backend::design::{Direction, Hop, HopKind, TraceStatus};

/// Decide the overall verdict for a set of hops.
///
/// `other` fetches the opposite direction. It is a closure rather than a value
/// because running it means a second full NPI traversal, and the call below is
/// arranged so that only queries which could actually come back
/// `TestbenchDriven` pay for it.
pub fn classify(
    hops: &[Hop],
    dir: Direction,
    other: impl FnOnce(Direction) -> Vec<Hop>,
) -> TraceStatus {
    if hops.is_empty() {
        return TraceStatus::NotFound;
    }
    if hops.iter().all(|h| h.kind == HopKind::Port) {
        // Everything we found is a hierarchy port: the actual driver is outside
        // the part of the design NPI followed.
        return TraceStatus::BoundaryOnly;
    }
    if dir != Direction::Driver {
        return TraceStatus::Resolved;
    }
    // The testbench-drive shape is *procedural* assignments only. A continuous
    // assign or a port is a structural driver by construction and can never be
    // the "this is really a reader" case, so skip the second traversal for it
    // — which is the common case, and the traversal is the expensive part.
    if !hops.iter().all(|h| h.kind == HopKind::Procedural) {
        return TraceStatus::Resolved;
    }
    // A statement that appears verbatim in both this net's driver list and its
    // load list is not telling us who drives the net — it is the shape a
    // testbench driving through a virtual interface leaves behind, where the
    // true driver lives in class-based code an RTL fan-in cannot see. This is
    // an identity test on (statement, file, line), not a name heuristic, so it
    // cannot fire on an ordinary self-referencing counter: `q <= q + 1` drives
    // through one statement and loads through a different one.
    let loads = other(Direction::Load);
    if !loads.is_empty() {
        let key = |h: &Hop| (h.statement.clone(), h.file.clone(), h.line);
        let load_keys: Vec<_> = loads.iter().map(key).collect();
        if hops.iter().all(|h| load_keys.contains(&key(h))) {
            return TraceStatus::TestbenchDriven;
        }
    }
    TraceStatus::Resolved
}

/// Map an NPI object type to the coarse kind reported to users.
pub fn kind_of(npi_type: &str) -> HopKind {
    match npi_type {
        "npiContAssign" => HopKind::ContAssign,
        "npiAssignment" | "npiAssignStmt" => HopKind::Procedural,
        "npiPort" | "npiMpPort" | "npiInstPort" => HopKind::Port,
        "npiIf" | "npiIfElse" | "npiCase" | "npiCaseItem" | "npiWhile" | "npiDoWhile"
        | "npiRepeat" | "npiForever" | "npiFor" | "npiWait" | "npiEventControl" => {
            HopKind::Control
        }
        "npiConstant" | "npiEnumConst" | "npiParameter" => HopKind::Constant,
        _ => HopKind::Other,
    }
}

/// Split a trailing `{<file> : <line>}` off a dump record.
///
/// Anchors on the `", {"` that separates the record's last field from its
/// location rather than on a bare `{`, so a statement that merely *ends* in a
/// brace — `q = '{default : 0}` — is not mistaken for a location.
fn split_location(s: &str) -> (String, Option<String>, Option<u32>) {
    let t = s.trim_end();
    if t.ends_with('}') {
        if let Some(open) = t.rfind(", {") {
            let inner = &t[open + 3..t.len() - 1];
            // Split on the last " : " so a path containing one keeps it.
            if let Some(sep) = inner.rfind(" : ") {
                if let Ok(line) = inner[sep + 3..].trim().parse::<u32>() {
                    let file = inner[..sep].trim().to_string();
                    return (t[..open].trim().to_string(), Some(file), Some(line));
                }
            }
        }
    }
    let head = t.trim_end_matches("(null)").trim_end().trim_end_matches(',').trim();
    (head.to_string(), None, None)
}

/// Parse one `TYPE, TEXT, {FILE : LINE}` record.
///
/// Splits on the *first* comma for the type and takes the location off the
/// *end*, so the statement text in between may contain commas of its own
/// (`q <= f(a, b);`), which naive comma-splitting would mangle.
fn parse_record(s: &str) -> (String, String, Option<String>, Option<u32>) {
    let (head, file, line) = split_location(s);
    match head.split_once(',') {
        Some((ty, rest)) => (
            ty.trim().to_string(),
            rest.trim().trim_end_matches(',').trim().to_string(),
            file,
            line,
        ),
        None => (head.trim().to_string(), String::new(), file, line),
    }
}

/// Is this a `<N> source: …, scope: …` group header?
fn group_header(line: &str) -> Option<(usize, String)> {
    let rest = line.strip_prefix('<')?;
    let close = rest.find('>')?;
    let n = rest[..close].parse::<usize>().ok()?;
    let scope = match line.find("scope:") {
        Some(i) => line[i + 6..].trim().to_string(),
        None => String::new(),
    };
    Some((n, scope))
}

/// Parse the text emitted by `npi_trace_{driver,load}_dump2` into hops.
pub fn parse_dump(text: &str) -> Vec<Hop> {
    let mut hops: Vec<Hop> = Vec::new();
    let mut group = 0usize;
    let mut scope = String::new();
    let mut boundary = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // A `<`-and-digit line is a group header. If it does not parse, reset
        // the group state rather than carrying the previous group's scope
        // forward — attributing hops to the wrong scope is worse than losing
        // the scope.
        if line.starts_with('<') && line[1..].starts_with(|c: char| c.is_ascii_digit()) {
            match group_header(line) {
                Some((n, sc)) => {
                    group = n;
                    scope = sc;
                }
                None => {
                    group += 1;
                    scope = String::new();
                }
            }
            // Pass-through state belongs to the group that declared it.
            boundary = false;
            continue;
        }
        if line.starts_with("pass through.") {
            // Applies to every statement in this group, until the next header.
            boundary = true;
            continue;
        }
        if line.starts_with("Need pass through")
            || line.starts_with("Do not pass through")
            || line.starts_with("Driver from sub stmts.")
            || line.starts_with("Load from sub stmts.")
            || line.contains("/* results of trace")
        {
            continue;
        }
        // `<D> …` / `<L> …` — a driving or loading statement. Tolerate any run
        // of whitespace after the marker, not exactly one space.
        if let Some(rest) = line.strip_prefix("<D>").or_else(|| line.strip_prefix("<L>")) {
            let (npi_type, statement, file, line_no) = parse_record(rest.trim_start());
            hops.push(Hop {
                group,
                kind: kind_of(&npi_type),
                npi_type,
                statement,
                scope: scope.clone(),
                file,
                line: line_no,
                boundary,
                signals: Vec::new(),
            });
            continue;
        }
        // Anything else at this point is an operand of the statement above.
        if let Some(h) = hops.last_mut() {
            let (npi_type, name, _file, _line) = parse_record(line);
            if npi_type.starts_with("npi") && !name.is_empty() {
                // Literals are not signals; recording them as endpoints would
                // put `'h00` in a list the caller looks values up by.
                if kind_of(&npi_type) != HopKind::Constant {
                    h.signals.push(name);
                }
            }
        }
    }
    hops
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim output from Verdi V-2023.12-SP2 for
    /// `npi_trace_driver_dump2("tb.u_core.u_alu.res", ...)`.
    const DRIVER_DUMP: &str = "\
npiReg, tb.u_core.u_alu.res, {/p/dut.sv : 15} /* results of trace driver */
Need pass through
<1> source: res, scope: tb.u_core.u_alu
    <D> npiContAssign, assign res = res_q, {/p/dut.sv : 27}
          npiReg, tb.u_core.u_alu.res_q, {/p/dut.sv : 17}
";

    /// Verbatim load output, exercising pass-through and a part-select.
    const LOAD_DUMP: &str = "\
npiReg, tb.u_core.u_alu.res, {/p/dut.sv : 15} /* results of trace load */
Need pass through
<1> source: res, scope: tb.u_core.u_alu
    pass through.
    <L> npiPort, res, {/p/dut.sv : 15}
          npiReg, tb.u_core.out, {/p/dut.sv : 34}
<2> source: out, scope: tb.u_core
    <L> npiAssignment, acc <= out[3:0];, {/p/dut.sv : 63}
          npiPartSelect, acc[3:0], {/p/dut.sv : 39}
";

    /// A control-dependency group, as emitted for an always_ff with async reset.
    const CONTROL_DUMP: &str = "\
npiEnumVar, tb.u_core.state, {/p/dut.sv : 38} /* results of trace driver */
Need pass through
<1> source: state, scope: tb.u_core
    <D> npiAssignment, state <= IDLE;, {/p/dut.sv : 43}
          npiEnumConst, IDLE, {/p/dut.sv : 37}
<2> source: state, scope: tb.u_core
    Driver from sub stmts.
    <D> npiEventControl, @(((posedge clk) or (negedge rst_n))), {/p/dut.sv : 42}
          npiNet, tb.u_core.rst_n, {/p/dut.sv : 33}
          npiNet, tb.u_core.clk, {/p/dut.sv : 32}
";

    #[test]
    fn parses_a_continuous_assignment_driver() {
        let hops = parse_dump(DRIVER_DUMP);
        assert_eq!(hops.len(), 1);
        let h = &hops[0];
        assert_eq!(h.kind, HopKind::ContAssign);
        assert_eq!(h.npi_type, "npiContAssign");
        assert_eq!(h.statement, "assign res = res_q");
        assert_eq!(h.scope, "tb.u_core.u_alu");
        assert_eq!(h.file.as_deref(), Some("/p/dut.sv"));
        assert_eq!(h.line, Some(27));
        assert!(!h.boundary);
        assert_eq!(h.signals, vec!["tb.u_core.u_alu.res_q"]);
    }

    #[test]
    fn marks_the_hop_that_crossed_a_port_boundary() {
        let hops = parse_dump(LOAD_DUMP);
        assert_eq!(hops.len(), 2);
        assert!(hops[0].boundary, "the pass-through hop must be flagged");
        assert_eq!(hops[0].kind, HopKind::Port);
        // The marker belongs to its own group and must not leak into the next.
        assert!(!hops[1].boundary);
        assert_eq!(hops[1].group, 2);
        assert_eq!(hops[1].statement, "acc <= out[3:0];");
    }

    #[test]
    fn classifies_enclosing_statements_as_control_not_data() {
        let hops = parse_dump(CONTROL_DUMP);
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].kind, HopKind::Procedural);
        assert_eq!(hops[1].kind, HopKind::Control);
        // Clock and reset arrive as control-group operands, so suppressing them
        // is a matter of not asking for control at all — never of matching
        // names like "rst", which would also eat `first_valid` and `burst_len`.
        assert_eq!(hops[1].signals, vec!["tb.u_core.rst_n", "tb.u_core.clk"]);
    }

    #[test]
    fn literals_are_not_reported_as_signal_endpoints() {
        let hops = parse_dump(CONTROL_DUMP);
        assert!(hops[0].signals.is_empty(), "got {:?}", hops[0].signals);
    }

    #[test]
    fn statement_text_may_contain_commas() {
        let dump = "\
<1> source: q, scope: tb
    <D> npiAssignment, q <= f(a, b);, {/p/dut.sv : 9}
          npiReg, tb.a, {/p/dut.sv : 3}
";
        let hops = parse_dump(dump);
        assert_eq!(hops[0].statement, "q <= f(a, b);");
        assert_eq!(hops[0].line, Some(9));
    }

    #[test]
    fn a_statement_ending_in_a_brace_is_not_read_as_a_location() {
        // `'{default : 0}` looks exactly like a `{file : line}` suffix unless
        // the parser anchors on the `", {"` separator.
        let dump = "\
<1> source: q, scope: tb
    <D> npiAssignment, q = '{default : 0}
";
        let hops = parse_dump(dump);
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].statement, "q = '{default : 0}");
        assert_eq!(hops[0].file, None);
        assert_eq!(hops[0].line, None);
    }

    #[test]
    fn a_record_without_a_location_still_parses() {
        let dump = "\
<1> source: q, scope: tb
    <D> npiAssignment, q = 0;, {/p/dut.sv : 9}
          npiConstant, 0, (null)
";
        let hops = parse_dump(dump);
        assert_eq!(hops.len(), 1);
        assert!(hops[0].signals.is_empty());
    }

    #[test]
    fn a_tab_after_the_marker_still_parses() {
        let dump = "<1> source: q, scope: tb\n    <D>\tnpiContAssign, assign q = a, {/p/d.sv : 4}\n";
        let hops = parse_dump(dump);
        assert_eq!(hops.len(), 1, "a tab must not make the statement vanish");
        assert_eq!(hops[0].statement, "assign q = a");
    }

    #[test]
    fn a_malformed_group_header_does_not_inherit_the_previous_scope() {
        // Misattributing a hop to the wrong scope is worse than losing the
        // scope, so the header state resets rather than carrying forward.
        let dump = "\
<1> source: a, scope: tb.u_first
    <D> npiContAssign, assign a = b, {/p/d.sv : 1}
<2 source: c, scope: tb.u_second
    <D> npiContAssign, assign c = d, {/p/d.sv : 2}
";
        let hops = parse_dump(dump);
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].scope, "tb.u_first");
        assert_eq!(hops[1].scope, "", "must not claim to be in tb.u_first");
    }

    #[test]
    fn crlf_and_odd_input_do_not_panic() {
        for s in [
            "",
            "<",
            "<>",
            "<é>",
            "<10> source: q, scope: tb\r\n    <D> npiReg, q, {/p/d.sv : 1}\r\n",
            "<1> source: q\n    <D> npiReg, q, (null)\n",
            "    npiReg, orphan, {/p/d.sv : 1}\n",
            "{",
            "}",
            "<1> source: 顶层, scope: 顶层.子模块\n    <D> npiReg, 顶层.x, {/p/d.sv : 1}\n",
        ] {
            let _ = parse_dump(s);
        }
        let hops = parse_dump(
            "<10> source: q, scope: tb\r\n    <D> npiReg, q, {/p/d.sv : 1}\r\n",
        );
        assert_eq!(hops[0].group, 10);
        assert_eq!(hops[0].scope, "tb");
    }

    #[test]
    fn empty_output_yields_no_hops() {
        assert!(parse_dump("").is_empty());
    }

    // -- classify -----------------------------------------------------------

    /// Panics if the cross-check traversal is run, so a test can assert that a
    /// verdict was reached without paying for the second NPI query.
    fn must_not_query(_d: Direction) -> Vec<Hop> {
        panic!("classify ran the opposite-direction traversal when it did not need to");
    }

    #[test]
    fn no_hops_reports_not_found() {
        assert_eq!(classify(&[], Direction::Driver, must_not_query), TraceStatus::NotFound);
    }

    #[test]
    fn all_port_hops_report_boundary_only() {
        let ports: Vec<Hop> = parse_dump(LOAD_DUMP)
            .into_iter()
            .filter(|h| h.kind == HopKind::Port)
            .collect();
        assert!(!ports.is_empty());
        assert_eq!(
            classify(&ports, Direction::Driver, must_not_query),
            TraceStatus::BoundaryOnly
        );
    }

    #[test]
    fn a_continuous_assign_driver_skips_the_cross_check_entirely() {
        // The second traversal is the expensive half of a trace; a structural
        // driver can never be the testbench-alias case, so it must not run.
        let drivers = parse_dump(DRIVER_DUMP);
        assert_eq!(drivers[0].kind, HopKind::ContAssign);
        assert_eq!(
            classify(&drivers, Direction::Driver, must_not_query),
            TraceStatus::Resolved
        );
    }

    #[test]
    fn a_load_query_never_runs_the_cross_check() {
        let hops = parse_dump(LOAD_DUMP);
        assert_eq!(classify(&hops, Direction::Load, must_not_query), TraceStatus::Resolved);
    }

    #[test]
    fn a_procedural_driver_that_is_also_a_load_of_the_same_net_is_reported_honestly() {
        let drivers = parse_dump(CONTROL_DUMP)
            .into_iter()
            .filter(|h| h.kind == HopKind::Procedural)
            .collect::<Vec<_>>();
        let loads = drivers.clone();
        assert_eq!(
            classify(&drivers, Direction::Driver, move |_| loads),
            TraceStatus::TestbenchDriven
        );
    }

    #[test]
    fn an_ordinary_procedural_driver_is_not_mistaken_for_testbench_drive() {
        let drivers = parse_dump(CONTROL_DUMP)
            .into_iter()
            .filter(|h| h.kind == HopKind::Procedural)
            .collect::<Vec<_>>();
        let loads = parse_dump(LOAD_DUMP);
        assert_eq!(
            classify(&drivers, Direction::Driver, move |_| loads),
            TraceStatus::Resolved
        );
    }
}
