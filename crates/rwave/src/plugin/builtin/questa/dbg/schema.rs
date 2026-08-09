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

pub fn port_links(db: &Db) -> Result<Vec<PortLink>, String> {
    rows(
        db,
        "SELECT DISTINCT loconn_net, hiconn_net, portmode FROM port_tbl",
        |r| Ok(PortLink { inner: r.get(0)?, outer: r.get(1)?, mode: r.get(2)? }),
    )
}

/// Nets collapsed into one by elaboration — an implicit wire and the net it
/// stands for. Not a port crossing, so it does not make a hop a boundary.
pub fn simnet_links(db: &Db) -> Result<Vec<(i64, i64)>, String> {
    rows(db, "SELECT anet_handle, fnet_handle FROM new_simnet_tbl", |r| {
        Ok((r.get(0)?, r.get(1)?))
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
    rows(
        db,
        &format!(
            "SELECT name, reader_incr_shapes, writer_incr_shapes \
             FROM signal_tbl WHERE du_id = {duid}"
        ),
        |r| {
            Ok(Signal {
                name: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                readers: shape_ids(&r.get::<_, Option<String>>(1)?.unwrap_or_default()),
                writers: shape_ids(&r.get::<_, Option<String>>(2)?.unwrap_or_default()),
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
        assert_eq!(operands("R:dbgTemp0_5"), vec!["dbgTemp0_5"]);
        assert_eq!(operands("rst_n en"), vec!["rst_n", "en"]);
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
}
