// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Typed reads of the tables `trace` needs. All SQL lives here.
//!
//! The layout is Questa's and undocumented, so each struct below records what a
//! column was observed to hold rather than what its name suggests. Two of those
//! observations are load-bearing and were wrong on first reading:
//!
//! - `shape_tbl.line` is **text**, and may list several lines (`'47 48 49'`)
//!   when one statement spans them. The first is the statement's own.
//! - a shape's enclosing statement is not its parent but the ancestor whose own
//!   parent is the `MODULE` shape; `GATE`/`FLOP`/`M` rows are primitives inside
//!   a `PROCESS`, and reporting one of those would name an expression rather
//!   than the assignment a reader is looking for.

use rusqlite::Row;

use super::open::Db;
use crate::plugin::builtin::questa::err;

fn rows<T, F>(db: &Db, sql: &str, f: F) -> Result<Vec<T>, String>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    let mut st = db
        .conn()
        .prepare(sql)
        .map_err(|e| err(format!("{}: {e}", db.path().display())))?;
    let it = st
        .query_map([], f)
        .map_err(|e| err(format!("{}: {e}", db.path().display())))?;
    it.collect::<rusqlite::Result<Vec<T>>>()
        .map_err(|e| err(format!("{}: {e}", db.path().display())))
}

// ---------------------------------------------------------------- top-level

/// A node of the design hierarchy. Processes are contexts too, which is what
/// lets an endpoint be named without a second lookup.
pub struct Context {
    pub handle: i64,
    pub parent: i64,
    pub name: String,
    pub du: i64,
}

/// A design unit. `vopt_duid` is the join to the per-module databases, whose
/// own tables key on it.
pub struct DesignUnit {
    pub handle: i64,
    pub vopt_duid: i64,
    pub library: String,
    pub name: String,
}

/// A port connection: the net inside the instance and the net outside it.
/// This is how a trace leaves one module and continues in another.
pub struct PortLink {
    pub inner: i64,
    pub outer: i64,
    /// 1 = input, 2 = output. Recorded but not used to pick a direction: which
    /// side drives is decided by which module actually writes the signal.
    pub mode: i64,
}

pub fn contexts(db: &Db) -> Result<Vec<Context>, String> {
    rows(
        db,
        "SELECT handle, scope_handle, name, du_handle FROM context_tbl",
        |r| {
            Ok(Context {
                handle: r.get(0)?,
                parent: r.get(1)?,
                name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                du: r.get(3)?,
            })
        },
    )
}

pub fn design_units(db: &Db) -> Result<Vec<DesignUnit>, String> {
    rows(
        db,
        "SELECT du_handle, vopt_duid, library_name, primary_name FROM du_tbl",
        |r| {
            Ok(DesignUnit {
                handle: r.get(0)?,
                vopt_duid: r.get(1)?,
                library: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        },
    )
}

/// Handles that are module instances rather than processes. A process carries
/// no definition, which is exactly what distinguishes the two.
pub fn instance_handles(db: &Db) -> Result<Vec<i64>, String> {
    rows(
        db,
        "SELECT inst_id FROM inst_tbl WHERE defn_du_id IS NOT NULL AND defn_du_id != 0",
        |r| r.get(0),
    )
}

/// `(instance, what it instantiates)`.
pub fn instance_defs(db: &Db) -> Result<Vec<(i64, i64)>, String> {
    rows(
        db,
        "SELECT inst_id, defn_du_id FROM inst_tbl \
         WHERE defn_du_id IS NOT NULL AND defn_du_id != 0",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// `(instance, port, the net that port is tied to)`.
///
/// An interface reached through a port is a second instance of the interface's
/// design unit, tied to the same nets through the same ports as the instance it
/// was passed. That pair of facts is what identifies the two as one thing —
/// nothing else in the database says so.
pub fn pins(db: &Db) -> Result<Vec<(i64, i64, i64)>, String> {
    rows(db, "SELECT inst_id, port_id, net_id FROM pin_tbl", |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
}

pub fn port_links(db: &Db) -> Result<Vec<PortLink>, String> {
    rows(
        db,
        "SELECT DISTINCT loconn_net, hiconn_net, portmode FROM port_tbl",
        |r| Ok(PortLink { inner: r.get(0)?, outer: r.get(1)?, mode: r.get(2)? }),
    )
}

/// Nets collapsed into one by elaboration — an implicit wire and the net it
/// stands for. Not a port crossing, so it does not make a hop a boundary.
///
/// Rows with a handle of `0` are placeholders, not aliases: a handle names a
/// context and `0` names none. On a large design there are thousands of them,
/// and taking them as edges makes `0` a hub joining every net they mention —
/// one 76 031-net "electrical net", which is where a config signal used to
/// come back with seventy thousand endpoints it had nothing to do with.
pub fn simnet_links(db: &Db) -> Result<Vec<(i64, i64)>, String> {
    rows(
        db,
        "SELECT anet_handle, fnet_handle FROM new_simnet_tbl \
         WHERE anet_handle != 0 AND fnet_handle != 0",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// `((net elaboration made, which bit of the named object), one name it has)`.
///
/// `port_tbl` records a connection per port, and following it level by level
/// reaches most of the hierarchy — but not all of it: a bus arriving at a leaf
/// module can have no row of its own, and the walk then stops one instance
/// short of the statement that reads it. This table is the flattened net, the
/// thing the simulator actually schedules, and every name it carries anywhere
/// in the design is listed against it.
///
/// A name can be a whole net in one row and bit 3 of a bus in another, so one
/// handle belongs to as many nets as the bus has bits. That is why this is not
/// an edge in the walk: joining bit 3's net to the handle and the handle to
/// bit 0's net merges two nets that share nothing but a name.
pub fn simnet_members(db: &Db) -> Result<Vec<(i64, i64)>, String> {
    rows(db, "SELECT simnet_id, net_handle FROM simnet_tbl", |r| Ok((r.get(0)?, r.get(1)?)))
}

/// `(statement, net it touches, whether it writes it)`, from the top level.
///
/// The per-module tables answer for anything the module declares, and that is
/// almost everything — but not a variable declared inside a named block, which
/// gets no `signal_tbl` row at all. This table names the pair directly and is
/// the only record of those.
///
/// The direction is the low bit of `flags`: odd writes, even reads. Established
/// against the module tables over half a million rows where they give a
/// definite answer, agreeing everywhere except on statements that do both.
pub fn proc_nets(db: &Db) -> Result<Vec<(i64, i64, bool)>, String> {
    rows(db, "SELECT proc_handle, net_handle, flags FROM proc_net_tbl", |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? & 1 == 1))
    })
}

// ------------------------------------------------------------- per-module

/// A signal as its own module sees it, with the shapes that touch it.
///
/// One name may appear several times with different bit ranges — a vector
/// written in two halves gets a row each — so a lookup unions their shapes.
pub struct Signal {
    pub name: String,
    pub readers: Vec<i64>,
    pub writers: Vec<i64>,
}

/// One node of a module's netlist. `MODULE` is the root, `PROCESS` a statement,
/// and `GATE`/`FLOP`/`M` the primitives inside one.
pub struct Shape {
    pub id: i64,
    pub parent: i64,
    pub kind: String,
    pub outputs: String,
    pub inputs: String,
    pub controls: String,
    /// For a `PROCESS`, the clock edge (`P:clk`); for a primitive, its
    /// expression text.
    pub spec1: String,
    /// For a `PROCESS`, its Questa name (`#p#47`, `#a#26`).
    pub spec2: String,
    pub file: i64,
    /// Every line this shape spans. A primitive lists one per assignment
    /// feeding it, which is what a reader wants named — the `always_ff` header
    /// above them is not where the value comes from.
    pub lines: Vec<u32>,
}

fn shape_ids(s: &str) -> Vec<i64> {
    s.split_whitespace().filter_map(|t| t.parse().ok()).collect()
}

/// The lines a shape spans, in order and without repeats.
fn lines_of(s: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for t in s.split_whitespace() {
        if let Ok(n) = t.parse::<u32>()
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    out
}

pub fn signals(db: &Db, duid: i64) -> Result<Vec<Signal>, String> {
    // Both pairs of columns, unioned. The `incr` one names the node the value
    // arrives at, which for an assignment inside a loop is the loop — reporting
    // it gives the reader the `for` header instead of the line that does the
    // work. The `caus` one names the primitive underneath, which carries the
    // assignment's own lines. Neither is a superset: a signal can have a node
    // in one and not the other.
    let union = |a: &str, b: &str| {
        let mut v = shape_ids(a);
        for id in shape_ids(b) {
            if !v.contains(&id) {
                v.push(id);
            }
        }
        v
    };
    rows(
        db,
        &format!(
            "SELECT name, reader_incr_shapes, writer_incr_shapes, \
                    reader_caus_shapes, writer_caus_shapes \
             FROM signal_tbl WHERE du_id = {duid}"
        ),
        |r| {
            let t = |i: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            Ok(Signal {
                name: t(0)?,
                readers: union(&t(1)?, &t(3)?),
                writers: union(&t(2)?, &t(4)?),
            })
        },
    )
}

pub fn shapes(db: &Db, duid: i64) -> Result<Vec<Shape>, String> {
    rows(
        db,
        &format!(
            "SELECT shape_id, parent_shape_id, type, outputs, inputs, controls, \
                    shape_specific_1, shape_specific_2, file, line \
             FROM shape_tbl WHERE du_id = {duid}"
        ),
        |r| {
            let text = |i: usize, r: &Row<'_>| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            Ok(Shape {
                id: r.get(0)?,
                parent: r.get(1)?,
                kind: text(2, r)?,
                outputs: text(3, r)?,
                inputs: text(4, r)?,
                controls: text(5, r)?,
                spec1: text(6, r)?,
                spec2: text(7, r)?,
                file: r.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                // Text, not an integer, and sometimes a list.
                lines: lines_of(&text(9, r)?),
            })
        },
    )
}

/// A statement as `rw_process_tbl` records it: what it reads and writes, by
/// name rather than by shape.
///
/// This is a second, independent view of the same thing `signal_tbl` gives, and
/// it is not redundant: a signal can have no shape recorded against it and
/// still be read here, which is the case `readers` answers and the shape lists
/// do not.
pub struct Process {
    pub name: String,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    /// Where the statement is. Recorded here as an integer, unlike
    /// `shape_tbl`, and the only location for a statement whose shape lists
    /// none.
    pub file: i64,
    pub line: Option<u32>,
}

/// Each entry carries a one-character tag, so a name is recovered by checking
/// the token against the module's own signal names before and after dropping
/// the first character. Guessing what the tag means would be a decode; this is
/// a lookup.
fn names(list: &str, known: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in list.split_whitespace() {
        // The list is Tcl: an element holding a bit-select is braced, so
        // `cam_inv_reset_val[0]` arrives as `{tcam_inv_reset_val[0]}`. Reading
        // it as one word drops it, and with it every vector reference in the
        // module — which is most of them.
        let tok = tok.strip_prefix('{').unwrap_or(tok);
        let tok = tok.strip_suffix('}').unwrap_or(tok);
        // Whole name first, then without the one-character kind prefix, then
        // the same two without the select. Trying the select last matters:
        // a signal can be declared with brackets in its name.
        let base = tok.split_once('[').map(|(b, _)| b).unwrap_or(tok);
        let Some(name) = [tok, &tok[1.min(tok.len())..], base, &base[1.min(base.len())..]]
            .into_iter()
            .find(|c| !c.is_empty() && known(c))
        else {
            continue;
        };
        if !out.iter().any(|x| x == name) {
            out.push(name.to_string());
        }
    }
    out
}

pub fn processes(
    db: &Db,
    duid: i64,
    known: &dyn Fn(&str) -> bool,
) -> Result<Vec<Process>, String> {
    rows(
        db,
        &format!(
            "SELECT name, readers, writers, file, line FROM rw_process_tbl WHERE duid = {duid}"
        ),
        |r| {
            let t = |i: usize, r: &Row<'_>| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
            };
            Ok((
                t(0, r)?,
                t(1, r)?,
                t(2, r)?,
                r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
                r.get::<_, Option<i64>>(4)?.filter(|l| *l > 0).map(|l| l as u32),
            ))
        },
    )
    .map(|v| {
        v.into_iter()
            .map(|(name, rd, wr, file, line)| Process {
                name,
                reads: names(&rd, known),
                writes: names(&wr, known),
                file,
                line,
            })
            .collect()
    })
}

/// The names the source actually declares, with where it declares them.
///
/// `signal_tbl` also holds the temporaries `vopt` invents to carry intermediate
/// values; those have no declaration, which is what separates them from the
/// signals someone wrote. Matching on their spelling would be a guess about a
/// naming convention — this is the record itself.
pub fn declared(db: &Db) -> Result<Vec<String>, String> {
    rows(db, "SELECT name FROM decl_tbl", |r| {
        Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default())
    })
}

/// Source file names, indexed by the `file` column of a shape (1-based).
pub fn files(db: &Db) -> Result<Vec<String>, String> {
    rows(db, "SELECT file_name FROM rw_file_tbl ORDER BY rowid", |r| {
        Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default())
    })
}

/// The logical library the design was optimised into, and the path to the
/// index database relative to it.
pub fn library(db: &Db) -> Result<Option<(String, String)>, String> {
    let v = rows(db, "SELECT logicalLib, dbPath FROM pdu_path_tbl", |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    Ok(v.into_iter().find(|(l, p)| !l.is_empty() && !p.is_empty()))
}

/// Where each design unit's own database lives, relative to the library.
pub struct DuFile {
    pub duid: i64,
    pub path: String,
}

pub fn du_files(db: &Db) -> Result<Vec<DuFile>, String> {
    rows(db, "SELECT duid, dbPath FROM rw_du_tbl", |r| {
        Ok(DuFile { duid: r.get(0)?, path: r.get::<_, Option<String>>(1)?.unwrap_or_default() })
    })
}

// ------------------------------------------------------------------ tokens

/// Split a `shape_tbl` operand list into signal names.
///
/// Tokens carry a bit range (`sum>7:0<`), and a control may carry an edge or
/// reset prefix (`P:clk`, `R:dbgTemp0_5`). Literals appear inline (`8'b00000000`,
/// `2'b00`) and are dropped: they are not signals, and the caller looks values
/// up by these names.
/// Whether a token is a signal name rather than a fragment of an expression.
///
/// Accepted by what a name may contain, not rejected by what an expression may:
/// a primitive's `shape_specific` columns hold source text that splits into
/// arbitrary tokens, and a list of forbidden characters would always be one
/// operator short. An escaped identifier is taken whole, since after the
/// backslash it may hold anything.
fn is_name(t: &str) -> bool {
    if t.is_empty() {
        return false;
    }
    if t.starts_with('\\') {
        return true;
    }
    let ok = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '/' | '.' | '[' | ']');
    t.chars().all(ok) && t.starts_with(|c: char| c.is_alphabetic() || matches!(c, '_' | '$' | '/'))
}

pub fn operands(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in list.split_whitespace() {
        let t = match tok.split_once(':') {
            // A one-letter tag is an edge or reset marker, not a scope.
            Some((tag, rest)) if tag.len() == 1 && !rest.is_empty() => rest,
            _ => tok,
        };
        let t = t.split('>').next().unwrap_or(t);
        if !is_name(t) {
            continue;
        }
        if !out.iter().any(|x: &String| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operands_drop_literals_and_strip_ranges() {
        // Verbatim from a PROCESS row: an always_ff with a reset value.
        assert_eq!(operands("sum>7:0< 8'b00000000"), vec!["sum"]);
        assert_eq!(operands("op_b>7:0< op_a>7:0<"), vec!["op_b", "op_a"]);
        assert_eq!(operands(""), Vec::<String>::new());
    }

    #[test]
    fn a_control_keeps_its_signal_not_its_edge_marker() {
        assert_eq!(operands("P:clk"), vec!["clk"]);
        assert_eq!(operands("rst_n en"), vec!["rst_n", "en"]);
        // The tag is stripped whatever follows it; that the temporary behind
        // this one is then dropped is a separate rule, checked below.
        assert_eq!(operands("N:rst_n"), vec!["rst_n"]);
    }

    #[test]
    fn an_interface_signal_keeps_its_separator() {
        // A module sees an interface member as `b/vld`; splitting that on the
        // colon rule above must not touch it.
        assert_eq!(operands("state>1:0< b/vld"), vec!["state", "b/vld"]);
    }

    #[test]
    fn expressions_are_not_mistaken_for_operands() {
        // A primitive's columns hold source text, which splits into tokens that
        // are not names. Verbatim from a GATE row.
        assert_eq!(operands("( ~(bool)(!!rst_n@@ ) )"), Vec::<String>::new());
        assert_eq!(operands("(!!free_cnt>7:0<@@  + 8'b00000001)"), Vec::<String>::new());
        assert_eq!(operands("!!res_q>7:0<@@ "), Vec::<String>::new());
    }

    #[test]
    fn an_escaped_identifier_survives_whole() {
        // After the backslash a Verilog name may hold anything, so it is taken
        // as-is rather than filtered character by character.
        assert_eq!(operands("\\foo.bar[3] clk"), vec!["\\foo.bar[3]", "clk"]);
    }

    #[test]
    fn process_entries_are_recovered_by_lookup_not_by_decoding_the_tag() {
        // Verbatim from an always_ff row: every name carries a one-character
        // tag, and `sum` would otherwise be lost as `tsum`.
        let known = |n: &str| ["clk", "rst_n", "en", "sum", "top_a"].contains(&n);
        assert_eq!(names("tclk trst_n trst_n ten tsum", &known), vec!["clk", "rst_n", "en", "sum"]);
        // A name that itself starts with the tag letter is found before the
        // tag is stripped, which is why this is a lookup and not a decode.
        assert_eq!(names("top_a", &known), vec!["top_a"]);
        assert_eq!(names("tnot_a_signal", &known), Vec::<String>::new());
    }

    #[test]
    fn a_repeated_operand_is_listed_once() {
        assert_eq!(operands("clk clk rst_n"), vec!["clk", "rst_n"]);
    }

    #[test]
    fn a_multi_line_statement_reports_where_it_starts() {
        assert_eq!(lines_of("47 48 49"), vec![47, 48, 49]);
        assert_eq!(lines_of("28 29 30 30"), vec![28, 29, 30], "repeats collapse");
        assert_eq!(lines_of("28"), vec![28]);
        assert_eq!(lines_of(""), Vec::<u32>::new());
        assert_eq!(shape_ids("37 34 9 8"), vec![37, 34, 9, 8]);
        assert_eq!(shape_ids(""), Vec::<i64>::new());
    }

    #[test]
    fn a_zero_handle_is_a_placeholder_not_an_alias() {
        // Verbatim shape from a real database: thousands of rows carry an
        // `anet_handle` of 0. A handle names a context and 0 names none, so
        // an edge through it would join every such row's net to every other.
        use super::super::open::tests::{tmp, write_dbg};
        use super::super::open::Kind;
        let d = tmp("simnet-zero");
        let p = d.join("run.dbg");
        write_dbg(
            &p,
            Kind::Top,
            6,
            &["CREATE TABLE new_simnet_tbl \
                (anet_handle, amsb, alsb, fnet_handle, fmsb, flsb, flags);
               INSERT INTO new_simnet_tbl VALUES (0, NULL, NULL, 77, NULL, NULL, 2);
               INSERT INTO new_simnet_tbl VALUES (0, NULL, NULL, 78, NULL, NULL, 4);
               INSERT INTO new_simnet_tbl VALUES (11, NULL, NULL, 22, NULL, NULL, 4);"],
        );
        let db = Db::open(&p, Kind::Top).unwrap();
        assert_eq!(simnet_links(&db).unwrap(), vec![(11, 22)]);
    }
}
