// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Answering driver and load questions from the databases.
//!
//! Two files hold complementary halves. The one beside the waveform has the
//! hierarchy and the wiring between modules; one per design unit has what the
//! statements inside that module read and write. A trace therefore goes:
//!
//! 1. the queried path names a context in the top-level database;
//! 2. port links and elaboration collapses give every other name for the same
//!    electrical net, in whatever modules it reaches;
//! 3. each of those names is looked up in its own module's database, where
//!    `signal_tbl` says which shapes write it and which read it;
//! 4. a shape is a primitive, so the walk goes up to the statement containing
//!    it, which is what a reader wants named.
//!
//! Step 2 is what makes `tb.u_core.out` answer with a statement in `u_alu`: the
//! two are one net across a port, and only the inner module writes it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::backend::design::{Direction, Hop, HopKind};
use crate::plugin::builtin::questa::err;

use super::open::{Db, Kind};
use super::schema;

/// The design, as far as the databases describe it.
pub struct Design {
    top: Db,
    /// Where the per-design-unit databases live, keyed by the id both files
    /// agree on.
    du_paths: HashMap<i64, PathBuf>,
    /// Opened on first use: a trace touches a handful of modules, not all.
    modules: HashMap<i64, Module>,

    ctx: HashMap<i64, Ctx>,
    /// Full path (as Questa spells it) to every context with that path.
    ///
    /// One name has several: a port declaration and the net behind it are
    /// separate contexts sharing a path, and the wiring tables reference
    /// whichever one they please. Keeping only the first silently loses every
    /// driver reached through a port.
    by_path: HashMap<String, Vec<i64>>,
    /// context handle -> the design unit its scope belongs to.
    du_of_ctx: HashMap<i64, i64>,
    /// Handles that are module instances rather than processes.
    instances: std::collections::HashSet<i64>,
    /// Net aliasing: handle -> the handles naming the same net, and whether
    /// reaching them crossed a port.
    links: HashMap<i64, Vec<(i64, bool)>>,
}

struct Ctx {
    parent: i64,
    name: String,
}

struct Module {
    signals: HashMap<String, (Vec<i64>, Vec<i64>)>,
    shapes: HashMap<i64, schema::Shape>,
    files: Vec<String>,
    /// Statement name (`#p#1402`) -> its shape. `rw_process_tbl` names
    /// statements; `shape_tbl` holds them.
    by_name: HashMap<String, i64>,
    /// Signal name -> the statements that read it, and that write it. A second
    /// view of the same fact, because a signal can carry no shape of its own
    /// and still be read.
    touched: HashMap<String, (Vec<String>, Vec<String>)>,
    /// Statement name -> where it is, for the statements whose shape records no
    /// line of its own.
    proc_loc: HashMap<String, (i64, Option<u32>)>,
}

impl Design {
    /// Open the top-level database and index what a trace needs from it.
    ///
    /// `lib_hint` overrides where the library holding `_dbcontainer` is; with
    /// `None` it is resolved from the database's own record of which library
    /// the design was optimised into.
    pub fn open(top_dbg: &Path, lib_hint: Option<&Path>) -> Result<Design, String> {
        let top = Db::open(top_dbg, Kind::Top)?;

        let mut ctx = HashMap::new();
        let mut du_of_ctx = HashMap::new();
        for c in schema::contexts(&top)? {
            du_of_ctx.insert(c.handle, c.du);
            ctx.insert(c.handle, Ctx { parent: c.parent, name: c.name });
        }
        let mut d = Design {
            top,
            du_paths: HashMap::new(),
            modules: HashMap::new(),
            ctx,
            by_path: HashMap::new(),
            du_of_ctx,
            instances: std::collections::HashSet::new(),
            links: HashMap::new(),
        };

        // Paths are built once: a design has one hierarchy and every lookup
        // wants the same spelling of it.
        let handles: Vec<i64> = d.ctx.keys().copied().collect();
        for h in handles {
            let p = d.path_of(h);
            d.by_path.entry(p).or_default().push(h);
        }
        d.instances = schema::instance_handles(&d.top)?.into_iter().collect();

        for l in schema::port_links(&d.top)? {
            d.links.entry(l.inner).or_default().push((l.outer, true));
            d.links.entry(l.outer).or_default().push((l.inner, true));
        }
        for (a, b) in schema::simnet_links(&d.top)? {
            // Not a port: an implicit wire and the net it stands for are the
            // same signal in the same module.
            d.links.entry(a).or_default().push((b, false));
            d.links.entry(b).or_default().push((a, false));
        }

        // The design-unit id is the join between the two files, and the module
        // databases are named by it.
        let lib_root = match lib_hint {
            Some(p) => p.to_path_buf(),
            None => resolve_library(&d.top, top_dbg)?,
        };
        let mti = find_mti(&lib_root.join("_dbcontainer"))?;
        let mti_db = Db::open(&mti, Kind::Index)?;
        for f in schema::du_files(&mti_db)? {
            d.du_paths.insert(f.duid, lib_root.join(&f.path));
        }
        Ok(d)
    }

    /// Questa's spelling of a context's path.
    fn path_of(&self, mut h: i64) -> String {
        let mut parts: Vec<&str> = Vec::new();
        while let Some(c) = self.ctx.get(&h) {
            if c.name.is_empty() || c.name == "/" {
                break;
            }
            parts.push(&c.name);
            h = c.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Every module instance that can name this context, innermost first, with
    /// the name it uses.
    ///
    /// Not just the innermost: an interface member is `data` inside the
    /// interface and `b/data` in the module the interface was declared in, and
    /// the statements reading it live in either. Stopping at the first instance
    /// loses the outer ones.
    fn owners(&self, h: i64) -> Vec<(i64, String)> {
        let mut out = Vec::new();
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = h;
        while let Some(c) = self.ctx.get(&cur) {
            if c.name.is_empty() || c.name == "/" {
                break;
            }
            if cur != h && self.instances.contains(&cur) {
                let mut p = parts.clone();
                p.reverse();
                out.push((cur, p.join("/")));
            }
            parts.push(&c.name);
            cur = c.parent;
        }
        out
    }

    fn module(&mut self, duid: i64) -> Result<&Module, String> {
        if !self.modules.contains_key(&duid) {
            let p = self
                .du_paths
                .get(&duid)
                .ok_or_else(|| err(format!("no database recorded for design unit {duid}")))?
                .clone();
            let db = Db::open(&p, Kind::Unit)?;
            let mut signals: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::new();
            for s in schema::signals(&db, duid)? {
                // One name can have a row per bit range; a query about the
                // signal means all of it.
                let e = signals.entry(s.name).or_default();
                e.0.extend(s.readers);
                e.1.extend(s.writers);
            }
            let shapes: HashMap<i64, schema::Shape> =
                schema::shapes(&db, duid)?.into_iter().map(|s| (s.id, s)).collect();
            let files = schema::files(&db)?;
            let by_name: HashMap<String, i64> = shapes
                .values()
                .filter(|s| !s.spec2.is_empty())
                .map(|s| (s.spec2.clone(), s.id))
                .collect();
            let known: std::collections::HashSet<String> = signals.keys().cloned().collect();
            let mut touched: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
            let mut proc_loc = HashMap::new();
            for p in schema::processes(&db, duid, &|n| known.contains(n))? {
                proc_loc.insert(p.name.clone(), (p.file, p.line));
                for r in p.reads {
                    touched.entry(r).or_default().0.push(p.name.clone());
                }
                for w in p.writes {
                    touched.entry(w).or_default().1.push(p.name.clone());
                }
            }
            self.modules
                .insert(duid, Module { signals, shapes, files, by_name, touched, proc_loc });
        }
        Ok(&self.modules[&duid])
    }

    /// Every name for the same electrical net, with whether reaching it crossed
    /// a port. Breadth-first so the nearest names come first.
    fn net_group(&self, start: i64) -> Vec<(i64, bool)> {
        let mut seen = HashMap::from([(start, false)]);
        let mut queue = std::collections::VecDeque::from([start]);
        let mut out = vec![(start, false)];
        while let Some(h) = queue.pop_front() {
            let crossed = seen[&h];
            for (next, port) in self.links.get(&h).into_iter().flatten() {
                let crossed = crossed || *port;
                if seen.contains_key(next) {
                    continue;
                }
                seen.insert(*next, crossed);
                out.push((*next, crossed));
                queue.push_back(*next);
            }
        }
        out
    }

    /// Whether the design knows this path at all — the difference between "no
    /// drivers" and "no such signal", which an empty answer cannot express.
    pub fn resolves(&self, questa_path: &str) -> bool {
        self.by_path.contains_key(questa_path)
    }

    /// Drivers or loads of `questa_path`.
    pub fn trace(
        &mut self,
        questa_path: &str,
        dir: Direction,
        control: bool,
    ) -> Result<Vec<Hop>, String> {
        let starts = self
            .by_path
            .get(questa_path)
            .cloned()
            .ok_or_else(|| err(format!("{questa_path} is not in the design database")))?;

        // Every context with this path, since the wiring may hang off any of
        // them; nearer names win when both reach the same statement.
        let mut group: Vec<(i64, bool)> = Vec::new();
        let mut have = std::collections::HashSet::new();
        for s in starts {
            for (h, crossed) in self.net_group(s) {
                if have.insert(h) {
                    group.push((h, crossed));
                }
            }
        }

        let mut hops = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (handle, crossed) in group {
            for (inst, local) in self.owners(handle) {
                let Some(&du_handle) = self.du_of_ctx.get(&inst) else { continue };
                let Some(duid) = self.duid_of(du_handle) else { continue };
                let inst_path = self.path_of(inst);

                let shape_ids = {
                    let m = self.module(duid)?;
                    let mut ids = match m.signals.get(&local) {
                        Some((readers, writers)) => match dir {
                            Direction::Driver => writers.clone(),
                            Direction::Load => readers.clone(),
                        },
                        None => Vec::new(),
                    };
                    // The statement view catches what the shape lists leave
                    // out: a signal with no shape recorded against it is still
                    // read by whatever statement names it.
                    if let Some((reads, writes)) = m.touched.get(&local) {
                        let names = match dir {
                            Direction::Driver => writes,
                            Direction::Load => reads,
                        };
                        for n in names {
                            if let Some(&id) = m.by_name.get(n)
                                && !ids.contains(&id)
                            {
                                ids.push(id);
                            }
                        }
                    }
                    if ids.is_empty() {
                        continue;
                    }
                    ids
                };
                for sid in shape_ids {
                    let m = &self.modules[&duid];
                    let Some(shape) = m.shapes.get(&sid) else { continue };
                    let Some(stmt) = statement_of(m, sid) else { continue };
                    // The primitive says which lines the value comes from; the
                    // statement above it says what kind of construct it is.
                    let mut lines: Vec<Option<u32>> = if !shape.lines.is_empty() {
                        shape.lines.iter().copied().map(Some).collect()
                    } else if !stmt.lines.is_empty() {
                        stmt.lines.iter().copied().map(Some).collect()
                    } else {
                        // Neither shape records one: the statement table does,
                        // as an integer. A hop with a file and no line prints an
                        // empty location, which is worse than looking it up.
                        m.proc_loc.get(&stmt.spec2).and_then(|(_, l)| *l).map(Some).into_iter().collect()
                    };
                    if lines.is_empty() {
                        lines.push(None);
                    }
                    for line in lines {
                        if !seen.insert((duid, stmt.id, inst_path.clone(), line)) {
                            continue;
                        }
                        hops.push(hop_of(m, stmt, shape, line, &inst_path, dir, control, crossed));
                    }
                }
            }
        }
        hops.sort_by(|a, b| (a.scope.clone(), a.line).cmp(&(b.scope.clone(), b.line)));
        for (i, h) in hops.iter_mut().enumerate() {
            h.group = i + 1;
        }
        Ok(hops)
    }

    fn duid_of(&self, du_handle: i64) -> Option<i64> {
        // Cached on first use; a design has a handful of units.
        schema::design_units(&self.top)
            .ok()?
            .into_iter()
            .find(|u| u.handle == du_handle)
            .map(|u| u.vopt_duid)
    }
}

/// The statement containing `shape`: walk up while the parent is another shape,
/// stopping below the `MODULE` root. A `GATE` names an expression; the
/// `PROCESS` above it names the assignment.
fn statement_of(m: &Module, mut id: i64) -> Option<&schema::Shape> {
    for _ in 0..64 {
        let s = m.shapes.get(&id)?;
        match m.shapes.get(&s.parent) {
            Some(p) if p.kind != "MODULE" => id = p.id,
            _ => return Some(s),
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn hop_of(
    m: &Module,
    s: &schema::Shape,
    prim: &schema::Shape,
    line: Option<u32>,
    inst_path: &str,
    dir: Direction,
    control: bool,
    crossed: bool,
) -> Hop {
    let scope = inst_path.trim_start_matches('/').replace('/', ".");
    let file = m
        .files
        .get(prim.file.max(s.file).saturating_sub(1) as usize)
        .filter(|f| !f.is_empty())
        .cloned();

    // A driver's operands are what it reads; a load's are what it writes.
    let mut signals: Vec<String> = match dir {
        Direction::Driver => schema::operands(&s.inputs),
        Direction::Load => schema::operands(&s.outputs),
    };
    if control {
        // The gating conditions live in two columns: `controls` holds the
        // enclosing condition, `spec1` the clock edge.
        for c in schema::operands(&s.controls).into_iter().chain(schema::operands(&s.spec1)) {
            if !signals.contains(&c) {
                signals.push(c);
            }
        }
    }
    let signals = signals.into_iter().map(|n| join(&scope, &n)).collect();

    Hop {
        group: 0,
        kind: kind_of(&s.kind, &s.spec2),
        raw_kind: if s.spec2.is_empty() { s.kind.clone() } else { s.spec2.clone() },
        // The database records a location but no text; the caller fills this in
        // from the source file when it can read it.
        statement: s.spec2.clone(),
        scope,
        file,
        line,
        // A port hop is a boundary by construction, whether or not the walk
        // to it crossed one.
        boundary: crossed || prim.kind == "INST" || s.kind == "INST",
        signals,
    }
}

/// A module-local name as a full rwave path. Interface members arrive with
/// their own separator, which becomes rwave's.
fn join(scope: &str, local: &str) -> String {
    let local = local.replace('/', ".");
    if scope.is_empty() { local } else { format!("{scope}.{local}") }
}

/// Questa's construct name decides the kind; its shape type is the fallback.
///
/// `#a#` is a continuous assignment and `#p#` a process — that is the same
/// distinction NPI draws between `npiContAssign` and `npiAssignment`, so the
/// two backends agree on what a driver *is* rather than only on where it is.
fn kind_of(shape_kind: &str, name: &str) -> HopKind {
    match name.split('#').nth(1).unwrap_or("") {
        "a" => return HopKind::ContAssign,
        "p" | "i" => return HopKind::Procedural,
        "w" => return HopKind::Other,
        _ => {}
    }
    match shape_kind {
        "PROCESS" | "FLOP" => HopKind::Procedural,
        // An instance and a module boundary are both ports: the value comes
        // from the other side of one.
        "INST" | "MODULE" => HopKind::Port,
        _ => HopKind::Gate,
    }
}

/// Where the library holding `_dbcontainer` is.
///
/// The database records the *logical* library name — `work` — because that is
/// what Questa resolves through `modelsim.ini`. The default mapping puts it in
/// a directory of that name beside the design, which is the case that needs no
/// help; an `.ini` that remaps it is read rather than guessed at.
fn resolve_library(top: &Db, top_dbg: &Path) -> Result<PathBuf, String> {
    let dir = top_dbg.parent().unwrap_or(Path::new("."));
    let name = schema::library(top)?.map(|(l, _)| l).ok_or_else(|| {
        err(format!(
            "{} does not record which library it was optimised into",
            top_dbg.display()
        ))
    })?;

    let plain = dir.join(&name);
    if plain.join("_dbcontainer").is_dir() {
        return Ok(plain);
    }
    if let Some(mapped) = library_from_ini(&dir.join("modelsim.ini"), &name) {
        let mapped = if mapped.is_absolute() { mapped } else { dir.join(mapped) };
        if mapped.join("_dbcontainer").is_dir() {
            return Ok(mapped);
        }
    }
    Err(err(format!(
        "cannot find the '{name}' library holding the per-module debug databases.          Looked for {}/_dbcontainer and in {}. rwave reads them from where the \
         design was optimised, so run it against a waveform in its own \
         simulation directory.",
        plain.display(),
        dir.join("modelsim.ini").display()
    )))
}

/// The `[Library]` mapping for `name`, if `modelsim.ini` gives one.
fn library_from_ini(ini: &Path, name: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(ini).ok()?;
    let mut in_section = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t.eq_ignore_ascii_case("[Library]");
            continue;
        }
        if !in_section || t.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=')
            && k.trim() == name
        {
            return Some(PathBuf::from(v.trim()));
        }
    }
    None
}

fn find_mti(container: &Path) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(container)
        .map_err(|e| err(format!("cannot read {}: {e}", container.display())))?;
    for e in entries.flatten() {
        let p = e.path().join("__mti.dbg");
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(err(format!(
        "{} holds no __mti.dbg, so the per-module databases cannot be found. \
         It is written by `vopt -debugdb` into the library the design was optimised in.",
        container.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statement_is_named_not_the_primitive_inside_it() {
        // A GATE inside a PROCESS inside the MODULE root: the answer a reader
        // wants is the PROCESS, which is the assignment they wrote.
        let mk = |id, parent, kind: &str, spec2: &str| schema::Shape {
            id,
            parent,
            kind: kind.into(),
            outputs: String::new(),
            inputs: String::new(),
            controls: String::new(),
            spec1: String::new(),
            spec2: spec2.into(),
            file: 1,
            lines: vec![33],
        };
        let m = Module {
            signals: HashMap::new(),
            by_name: HashMap::new(),
            touched: HashMap::new(),
            proc_loc: HashMap::new(),
            files: vec!["dut.sv".into()],
            shapes: HashMap::from([
                (1, mk(1, 0, "MODULE", "alu")),
                (11, mk(11, 1, "PROCESS", "#a#33")),
                (12, mk(12, 11, "GATE", "")),
            ]),
        };
        assert_eq!(statement_of(&m, 12).unwrap().id, 11);
        assert_eq!(statement_of(&m, 11).unwrap().id, 11);
    }

    #[test]
    fn a_cycle_in_the_shape_tree_terminates() {
        let mk = |id, parent| schema::Shape {
            id,
            parent,
            kind: "GATE".into(),
            outputs: String::new(),
            inputs: String::new(),
            controls: String::new(),
            spec1: String::new(),
            spec2: String::new(),
            file: 1,
            lines: Vec::new(),
        };
        let m = Module {
            signals: HashMap::new(),
            by_name: HashMap::new(),
            touched: HashMap::new(),
            proc_loc: HashMap::new(),
            files: vec![],
            shapes: HashMap::from([(1, mk(1, 2)), (2, mk(2, 1))]),
        };
        assert!(statement_of(&m, 1).is_none(), "a malformed tree must not hang");
    }

    #[test]
    fn kinds_follow_questas_own_construct_names() {
        assert_eq!(kind_of("PROCESS", "#a#26"), HopKind::ContAssign);
        assert_eq!(kind_of("PROCESS", "#p#28"), HopKind::Procedural);
        assert_eq!(kind_of("PROCESS", "#i#16"), HopKind::Procedural);
        // No construct name: fall back to what the netlist says it is.
        assert_eq!(kind_of("GATE", ""), HopKind::Gate);
        assert_eq!(kind_of("FLOP", ""), HopKind::Procedural);
        assert_eq!(kind_of("MODULE", "alu"), HopKind::Port);
    }

    #[test]
    fn module_local_names_become_rwave_paths() {
        assert_eq!(join("tb.u_core", "acc"), "tb.u_core.acc");
        // An interface member is `b/vld` inside its module.
        assert_eq!(join("tb.u_core", "b/vld"), "tb.u_core.b.vld");
        assert_eq!(join("", "clk"), "clk");
    }
}
