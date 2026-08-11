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

// Handles are raw 64-bit pointers and the name maps are hot: SipHash on an
// 8-byte key is most of the cost of building the index, and this file never
// hashes anything an outsider chose. Same reason `model` uses it.
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
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
    /// Scope handle -> the contexts directly inside it, and the handles no
    /// enclosing scope names.
    ///
    /// A path is resolved by walking these down one component at a time rather
    /// than by looking up a string. The design has one context per named thing,
    /// and on a large one that is 1.7 million of them; spelling out every full
    /// path costs 300 MB of strings to build and 215 MB to keep, which is more
    /// than the rest of the index put together and buys nothing a descent does
    /// not do. Several contexts can share a path — a port declaration and the
    /// net behind it are separate, and the wiring tables reference whichever
    /// they please — so a walk keeps every match at each level.
    children: HashMap<i64, Vec<i64>>,
    roots: Vec<i64>,
    /// Last component -> the contexts with that name, for the suffix match.
    by_leaf: HashMap<String, Vec<i64>>,
    /// context handle -> the design unit its scope belongs to.
    du_of_ctx: HashMap<i64, i64>,
    /// design-unit handle -> the `vopt` id the per-module databases key on.
    /// Built once: a trace asks this per instance it walks through, and on a
    /// design with a thousand units a scan each time is the cost of the query.
    duid_of_handle: HashMap<i64, i64>,
    /// Handles that are module instances rather than processes.
    instances: HashSet<i64>,
    /// Net aliasing: handle -> the handles naming the same net, and whether
    /// reaching them crossed a port.
    links: HashMap<i64, Vec<(i64, bool)>>,
    /// handle -> the elaborated nets it takes part in, and each net -> every
    /// name it has. Consulted for the signal a query names, not walked.
    simnet_of: HashMap<i64, Vec<i64>>,
    simnet_members: HashMap<i64, Vec<i64>>,
    /// A vector alias -> the per-bit nets it was split or gathered into, and
    /// each of those nets -> the vectors naming it. The bits are different
    /// nets, so these are not edges: a walk entering the vector on one bit
    /// would leave on every other. The vector's bits are walked when the
    /// vector itself is asked about, and the vector is consulted — name and
    /// neighbours, one hop — when one of its bits is.
    vector_bits: HashMap<i64, Vec<i64>>,
    vector_of: HashMap<i64, Vec<i64>>,
    /// net handle -> the statements touching it and whether they write it.
    /// The last resort, for what the module tables do not record at all.
    proc_of_net: HashMap<i64, Vec<(i64, bool)>>,
    /// `(design unit, statement name)` -> every place it sits, as a path of
    /// blocks relative to the module. The module databases name a statement
    /// without them, and a nested generate runs the same statement once per
    /// branch combination — `gen_a[1]/gen_b[2]` — so this is a list.
    block_of: HashMap<(i64, String), Vec<String>>,
    /// An interface port -> the interface instance it was passed, and back.
    /// The port is a context of its own with no members under it; the members
    /// are under the instance, so a path through the port has to change name
    /// halfway down.
    alias_of: HashMap<i64, Vec<i64>>,
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
    /// The same, for the statements `shape_tbl` names with the generate block
    /// they sit in. One trailing name can belong to several shapes — a `for`
    /// generate replicates the statement once per branch — and all of them are
    /// that statement.
    by_tail: HashMap<String, Vec<i64>>,
    /// Signal name -> the statements that read it, and that write it. A second
    /// view of the same fact, because a signal can carry no shape of its own
    /// and still be read.
    touched: HashMap<String, (Vec<String>, Vec<String>)>,
    /// Statement name -> where it is, for the statements whose shape records no
    /// line of its own.
    proc_loc: HashMap<String, (i64, Option<u32>)>,
    /// Bare statement name -> the name with the block label the process table
    /// records for it. `shape_tbl` names the same statement `#p#129` where
    /// `rw_process_tbl` calls it `#p#129(input_translation)`, and the label is
    /// what someone reading the RTL recognises.
    labelled: HashMap<String, String>,
    /// What follows the tag in a statement's name -> the one statement with
    /// that suffix, where there is only one. The hierarchy and the module
    /// tables tag a statement differently and agree on everything after it,
    /// so this finds a construct the tag rewrite has never seen.
    proc_by_rest: HashMap<String, (String, (i64, Option<u32>))>,
    /// The names the source declares. Anything else in this module is a `vopt`
    /// temporary: real to the netlist, absent from the RTL and from the
    /// waveform, and not something to hand back as an endpoint.
    declared: HashSet<String>,
}

impl Design {
    /// Open the top-level database and index what a trace needs from it.
    ///
    /// `lib_hint` overrides where the library holding `_dbcontainer` is; with
    /// `None` it is resolved from the database's own record of which library
    /// the design was optimised into.
    pub fn open(top_dbg: &Path, lib_hint: Option<&Path>) -> Result<Design, String> {
        let top = Db::open(top_dbg, Kind::Top)?;

        let mut ctx = HashMap::default();
        let mut du_of_ctx = HashMap::default();
        for c in schema::contexts(&top)? {
            du_of_ctx.insert(c.handle, c.du);
            ctx.insert(c.handle, Ctx { parent: c.parent, name: c.name });
        }
        let duid_of_handle = schema::design_units(&top)?
            .into_iter()
            .map(|u| (u.handle, u.vopt_duid))
            .collect();
        let mut d = Design {
            top,
            du_paths: HashMap::default(),
            modules: HashMap::default(),
            ctx,
            children: HashMap::default(),
            roots: Vec::new(),
            by_leaf: HashMap::default(),
            du_of_ctx,
            duid_of_handle,
            instances: HashSet::default(),
            links: HashMap::default(),
            simnet_of: HashMap::default(),
            simnet_members: HashMap::default(),
            vector_bits: HashMap::default(),
            vector_of: HashMap::default(),
            proc_of_net: HashMap::default(),
            block_of: HashMap::default(),
            alias_of: HashMap::default(),
        };

        // The hierarchy index: which contexts sit inside which, and which name
        // no enclosing one. A context whose parent is absent, or whose parent
        // is the unnamed root, is where a path starts — the same condition
        // `path_of` stops climbing on.
        for (&h, c) in &d.ctx {
            if c.name.is_empty() || c.name == "/" {
                continue;
            }
            match d.ctx.get(&c.parent) {
                Some(p) if !p.name.is_empty() && p.name != "/" => {
                    d.children.entry(c.parent).or_default().push(h)
                }
                _ => d.roots.push(h),
            }
            d.by_leaf.entry(c.name.clone()).or_default().push(h);
        }
        d.instances = schema::instance_handles(&d.top)?.into_iter().collect();
        // `inst_tbl` does not list every instance. On a design that leans on
        // parameterised leaf modules it names well under half of them, and what
        // it leaves out are ordinary instances — `rvdff dffs (...)` inside
        // `rvdffs`, the flop every VeeR register is built from. A context whose
        // design unit is not its parent's is an instance boundary by
        // construction, which is the fact a name lookup needs: without it the
        // signal inside can only be named from the level above, as `dffs/dout`,
        // and the module holding the statement that drives it does not know
        // that name. The answer comes back as a chain of port hops that stops
        // one level short of the `always_ff`.
        let boundaries: Vec<i64> = d
            .ctx
            .iter()
            .filter(|(h, c)| {
                !c.name.is_empty()
                    && c.name != "/"
                    && d.du_of_ctx.get(*h).is_some_and(|&du| {
                        du != 0 && d.du_of_ctx.get(&c.parent).is_some_and(|&p| p != du)
                    })
            })
            .map(|(h, _)| *h)
            .collect();
        d.instances.extend(boundaries);

        // The elaborated net, which names the connection the port table stops
        // short of. The groups come first because they carry two facts the
        // edges below turn on: which handles are more than one bit wide, and
        // which nets have a group at all.
        let mut seen_bit: HashMap<i64, i64> = HashMap::default();
        let mut multibit: HashSet<i64> = HashSet::default();
        for (id, net, bit) in schema::simnet_members(&d.top)? {
            d.simnet_of.entry(net).or_default().push(id);
            d.simnet_members.entry(id).or_default().push(net);
            if let Some(b) = bit {
                match seen_bit.get(&net) {
                    Some(&prev) if prev != b => {
                        multibit.insert(net);
                    }
                    Some(_) => {}
                    None => {
                        seen_bit.insert(net, b);
                    }
                }
            }
        }
        // Ports connect objects, and where one end is a vector and the other
        // a bit of it, the object edge joins every bit's net through the
        // vector node — two generate branches each drive their own net, the
        // parent gathers the pair, and either branch used to answer with
        // both. The bit's real neighbours are its simulation net, chained
        // below, so the mixed edge is dropped wherever that net is recorded
        // — and kept where it is not, as the only record there is.
        for l in schema::port_links(&d.top)? {
            if multibit.contains(&l.inner) != multibit.contains(&l.outer) {
                let scalar = if multibit.contains(&l.inner) { l.outer } else { l.inner };
                if d.simnet_of.contains_key(&scalar) {
                    continue;
                }
            }
            d.links.entry(l.inner).or_default().push((l.outer, true));
            d.links.entry(l.outer).or_default().push((l.inner, true));
        }
        // The scalars of one simulation net are one electrical net, however
        // far apart the hierarchy put them. The vector members stay out — a
        // vector belongs to one net per bit — and are consulted at query
        // time instead.
        for members in d.simnet_members.values() {
            let mut sc: Vec<i64> =
                members.iter().copied().filter(|h| !multibit.contains(h)).collect();
            sc.sort_unstable();
            sc.dedup();
            for w in sc.windows(2) {
                d.links.entry(w[0]).or_default().push((w[1], true));
                d.links.entry(w[1]).or_default().push((w[0], true));
            }
        }
        // An alias and the net it stands for are the same signal — when they
        // are one thing. A vector alias is many things, one per bit, and its
        // rows must not become edges of one node; `split_aliases` says which
        // are which.
        let (plain, vector_bits, vector_of) =
            split_aliases(schema::simnet_links(&d.top)?, &multibit);
        for (a, b) in plain {
            if multibit.contains(&a) != multibit.contains(&b) {
                let scalar = if multibit.contains(&a) { b } else { a };
                if d.simnet_of.contains_key(&scalar) {
                    continue;
                }
            }
            d.links.entry(a).or_default().push((b, false));
            d.links.entry(b).or_default().push((a, false));
        }
        d.vector_bits = vector_bits;
        d.vector_of = vector_of;
        for (proc, net, writes) in schema::proc_nets(&d.top)? {
            d.proc_of_net.entry(net).or_default().push((proc, writes));
        }
        // Instances of one design unit tied to the same nets through the same
        // ports are one elaborated thing under two names — an interface and the
        // port some module received it on. Pins are what distinguishes them
        // from two ordinary instances of the same module, so an instance with
        // none is not matched on at all.
        let mut pins: HashMap<i64, Vec<(i64, i64)>> = HashMap::default();
        for (inst, port, net) in schema::pins(&d.top)? {
            pins.entry(inst).or_default().push((port, net));
        }
        let mut same: HashMap<(i64, Vec<(i64, i64)>), Vec<i64>> = HashMap::default();
        for (inst, defn) in schema::instance_defs(&d.top)? {
            let Some(p) = pins.get(&inst) else { continue };
            let mut p = p.clone();
            p.sort_unstable();
            p.dedup();
            same.entry((defn, p)).or_default().push(inst);
        }
        for (_, group) in same {
            if group.len() < 2 {
                continue;
            }
            for &a in &group {
                d.alias_of.entry(a).or_default().extend(group.iter().copied().filter(|&b| b != a));
            }
        }
        // An interface port with no pins to match on. Questa writes a port's
        // `inst_tbl` and `pin_tbl` rows for only one of a module's
        // instantiations — where one module is instantiated twice, the port
        // exists under both and only one has them — so the pass above cannot
        // see the other. The binding is still in the database: elaboration collapses
        // an interface's member nets into nets of the module that received
        // it, so the members of the interface actually passed share a
        // simulation net with something inside this port's module, and the
        // members of every other instance of that interface do not.
        //
        // Only a childless instance boundary without pins is a port in
        // absentia — a real instance keeps its pins even when optimisation
        // empties it — and only a unique candidate is taken: two interfaces
        // of one type wired into the same module cannot be told apart
        // without pins, and a wrong binding is worse than no answer.
        let mut orphans: Vec<(i64, i64, i64)> = Vec::new();
        for (&h, c) in &d.ctx {
            if c.name.is_empty() || c.name == "/" || c.name.starts_with('#') {
                continue;
            }
            if d.children.contains_key(&h) || d.alias_of.contains_key(&h) || pins.contains_key(&h)
            {
                continue;
            }
            let Some(&du) = d.du_of_ctx.get(&h) else { continue };
            let Some(&parent_du) = d.du_of_ctx.get(&c.parent) else { continue };
            if du == 0 || parent_du == du {
                continue;
            }
            orphans.push((h, du, c.parent));
        }
        let wanted: HashSet<i64> = orphans.iter().map(|&(_, du, _)| du).collect();
        let mut elaborated: HashMap<i64, Vec<i64>> = HashMap::default();
        for (&h, c) in &d.ctx {
            let Some(&du) = d.du_of_ctx.get(&h) else { continue };
            if !wanted.contains(&du) || !d.children.contains_key(&h) {
                continue;
            }
            if d.du_of_ctx.get(&c.parent).is_some_and(|&p| p != du) {
                elaborated.entry(du).or_default().push(h);
            }
        }
        for (h, du, scope) in orphans {
            let hits: Vec<i64> = elaborated
                .get(&du)
                .into_iter()
                .flatten()
                .copied()
                // An enclosing scope is where the port lives, not what it
                // was passed.
                .filter(|&cand| !d.encloses(cand, h) && d.collapsed_into(cand, scope))
                .collect();
            if let &[target] = hits.as_slice() {
                d.alias_of.entry(h).or_default().push(target);
                d.alias_of.entry(target).or_default().push(h);
            }
        }
        // Which blocks a statement sits inside, as a path relative to the
        // module. The module databases name a statement on its own; the design
        // spells it with the generate branch in front, and that branch is the
        // same in every instance of the module, so this is keyed by the design
        // unit rather than by instance. Walking up stops where the design unit
        // changes, which is the instance boundary.
        let statements: Vec<(i64, i64, String)> = d
            .ctx
            .iter()
            .filter(|(_, c)| c.name.starts_with('#'))
            .filter_map(|(&h, c)| d.du_of_ctx.get(&h).map(|&du| (h, du, construct_name(&c.name))))
            .collect();
        for (h, du, name) in statements {
            let mut parts: Vec<&str> = Vec::new();
            let mut cur = d.ctx[&h].parent;
            while let Some(c) = d.ctx.get(&cur) {
                if d.du_of_ctx.get(&cur) != Some(&du) || d.du_of_ctx.get(&c.parent) != Some(&du) {
                    break;
                }
                parts.push(&c.name);
                cur = c.parent;
            }
            if !parts.is_empty() {
                parts.reverse();
                let at = parts.join("/");
                let seen = d.block_of.entry((du, name)).or_default();
                if !seen.contains(&at) {
                    seen.push(at);
                }
            }
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
                // Which separator a module uses for a nested name is not
                // consistent: the interface `b` reaches its member as `b/vld`
                // in one module and `b.data` in another. Offer both rather
                // than pick.
                out.push((cur, p.join("/")));
                if p.len() > 1 {
                    out.push((cur, p.join(".")));
                }
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
            let mut signals: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::default();
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
            // The two tables spell a statement inside a generate block
            // differently: `shape_tbl` puts the block in front of it,
            // `gen_counters[0]/#p#643`, and `rw_process_tbl` names it `#p#643`.
            // The statement view looks statements up by what the process table
            // calls them, so without this the shapes in every generate block
            // are unreachable from that direction.
            //
            // Several shapes can share the trailing name, and that is not an
            // ambiguity to refuse: a `for` generate writes the statement once
            // and elaboration keeps a node per branch, so `mem_bank[0]/#a#105`
            // through `mem_bank[7]/#a#105` are eight places the same source
            // line runs and all eight are answers. Each carries its own branch
            // in the name it is reported under.
            let mut by_tail: HashMap<String, Vec<i64>> = HashMap::default();
            for s in shapes.values() {
                if let Some((_, t)) = s.spec2.rsplit_once('/') {
                    by_tail.entry(t.to_string()).or_default().push(s.id);
                }
            }
            for ids in by_tail.values_mut() {
                ids.sort_unstable();
            }
            let known: HashSet<String> = signals.keys().cloned().collect();
            let mut touched: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::default();
            let mut proc_loc = HashMap::default();
            for p in schema::processes(&db, duid, &|n| known.contains(n))? {
                proc_loc.insert(p.name.clone(), (p.file, p.line));
                for r in p.reads {
                    touched.entry(r).or_default().0.push(p.name.clone());
                }
                for w in p.writes {
                    touched.entry(w).or_default().1.push(p.name.clone());
                }
            }
            let labelled: HashMap<String, String> = proc_loc
                .keys()
                .filter(|n| n.ends_with(')'))
                .map(|n| (label_stripped(n).to_string(), n.clone()))
                .collect();
            let mut by_rest: HashMap<String, Vec<(String, (i64, Option<u32>))>> =
                HashMap::default();
            for (n, &loc) in &proc_loc {
                if let Some(rest) = rest_of(n) {
                    by_rest.entry(rest.to_string()).or_default().push((n.clone(), loc));
                }
            }
            // Two statements can share a suffix — a line with both an assign
            // and a process on it — and then it identifies neither.
            let proc_by_rest: HashMap<String, (String, (i64, Option<u32>))> = by_rest
                .into_iter()
                .filter_map(|(k, mut v)| (v.len() == 1).then(|| (k, v.remove(0))))
                .collect();
            let declared = schema::declared(&db)?.into_iter().collect();
            self.modules.insert(
                duid,
                Module {
                    signals,
                    shapes,
                    files,
                    by_name,
                    by_tail,
                    touched,
                    proc_loc,
                    labelled,
                    proc_by_rest,
                    declared,
                },
            );
        }
        Ok(&self.modules[&duid])
    }

    /// Every name for the same electrical net, with whether reaching it crossed
    /// a port. Breadth-first so the nearest names come first.
    fn net_group(&self, start: i64) -> Vec<(i64, bool)> {
        let mut seen = HashMap::default();
        seen.insert(start, false);
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

    /// The same signal reached by a shorter path, when exactly one exists.
    ///
    /// An interface accessed through a modport port is named for the port in
    /// the waveform — `tb.u_core.b.clk` — and for its own instance in the
    /// design — `/tb/b/clk`. Matching on the trailing components finds it. Only
    /// a unique match is taken: two candidates mean the shorter path does not
    /// identify which one was asked for, and answering about the wrong signal
    /// is worse than saying the name did not resolve.
    fn by_suffix(&self, questa_path: &str) -> Option<Vec<i64>> {
        let segs: Vec<&str> = questa_path.trim_start_matches('/').split('/').collect();
        for take in (2..segs.len()).rev() {
            let tail = &segs[segs.len() - take..];
            // Candidates come from the last component, which is what makes this
            // a lookup rather than a scan of every path in the design.
            let hits: Vec<i64> = self
                .by_leaf
                .get(tail[tail.len() - 1])
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .copied()
                .filter(|&h| self.ends_with(h, tail))
                .collect();
            let mut paths: Vec<String> = hits.iter().map(|&h| self.path_of(h)).collect();
            paths.sort();
            paths.dedup();
            paths.retain(|p| p != questa_path);
            if paths.len() == 1 {
                let want = &paths[0];
                return Some(hits.into_iter().filter(|&h| self.path_of(h) == *want).collect());
            }
            if paths.len() > 1 {
                return None;
            }
        }
        None
    }

    /// Whether `h`'s own path ends with these components.
    fn ends_with(&self, mut h: i64, tail: &[&str]) -> bool {
        for seg in tail.iter().rev() {
            match self.ctx.get(&h) {
                Some(c) if c.name == *seg => h = c.parent,
                _ => return false,
            }
        }
        true
    }

    /// Whether `anc` is `h` or a scope enclosing it.
    fn encloses(&self, anc: i64, mut h: i64) -> bool {
        loop {
            if h == anc {
                return true;
            }
            match self.ctx.get(&h) {
                Some(c) => h = c.parent,
                None => return false,
            }
        }
    }

    /// Whether elaboration collapsed a net inside `inst` with one inside
    /// `scope`. This is the fact that binds an interface to the port it was
    /// passed on, recorded net by net rather than as a row about either: the
    /// receiving module wires members to its own contents, and those joins
    /// all land in the simulation-net table.
    fn collapsed_into(&self, inst: i64, scope: i64) -> bool {
        let mut stack = vec![inst];
        while let Some(x) = stack.pop() {
            if let Some(kids) = self.children.get(&x) {
                stack.extend(kids);
            }
            if x == inst {
                continue;
            }
            for id in self.simnet_of.get(&x).into_iter().flatten() {
                for &m in self.simnet_members.get(id).into_iter().flatten() {
                    if self.encloses(scope, m) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Every context whose path is exactly this, walking the hierarchy down one
    /// component at a time.
    fn resolve(&self, questa_path: &str) -> Vec<i64> {
        let mut segs = questa_path.trim_start_matches('/').split('/');
        let Some(first) = segs.next() else { return Vec::new() };
        let mut cur: Vec<i64> =
            self.roots.iter().copied().filter(|h| self.ctx[h].name == first).collect();
        for seg in segs {
            let mut next = self.step(&cur, seg);
            if next.is_empty() {
                // The name may be a port the interface was passed on, which
                // holds no members of its own; they are under the instance.
                let aliases: Vec<i64> =
                    cur.iter().filter_map(|h| self.alias_of.get(h)).flatten().copied().collect();
                next = self.step(&aliases, seg);
            }
            if next.is_empty() {
                return Vec::new();
            }
            cur = next;
        }
        cur
    }

    /// The contexts named `seg` directly inside any of `from`.
    fn step(&self, from: &[i64], seg: &str) -> Vec<i64> {
        let mut out = Vec::new();
        for h in from {
            for &c in self.children.get(h).map(Vec::as_slice).unwrap_or_default() {
                if self.ctx.get(&c).is_some_and(|x| x.name == seg) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// Whether the design knows this path at all — the difference between "no
    /// drivers" and "no such signal", which an empty answer cannot express.
    pub fn resolves(&self, questa_path: &str) -> bool {
        !self.resolve(questa_path).is_empty() || self.by_suffix(questa_path).is_some()
    }

    /// Drivers or loads of `questa_path`.
    pub fn trace(
        &mut self,
        questa_path: &str,
        dir: Direction,
        control: bool,
    ) -> Result<Vec<Hop>, String> {
        let starts = Some(self.resolve(questa_path))
            .filter(|v| !v.is_empty())
            .or_else(|| self.by_suffix(questa_path))
            .ok_or_else(|| err(format!("{questa_path} is not in the design database")))?;

        // Every context with this path, since the wiring may hang off any of
        // them; nearer names win when both reach the same statement.
        let mut group: Vec<(i64, bool)> = Vec::new();
        let mut have = HashSet::default();
        for s in &starts {
            for (h, crossed) in self.net_group(*s) {
                if have.insert(h) {
                    group.push((h, crossed));
                }
            }
            // The bits of a vector asked about by name are the signal in
            // hand: each one's net is walked like the vector's own.
            for f in self.vector_bits.get(s).into_iter().flatten() {
                for (h, crossed) in self.net_group(*f) {
                    if have.insert(h) {
                        group.push((h, crossed));
                    }
                }
            }
        }
        // Every other name the elaborated net has, for the signal that was
        // asked about. This is where a connection the port table never spells
        // out comes from — a bus arriving at a leaf module — and it is applied
        // to the query's own handles only, never followed onward: the net a
        // handle belongs to depends on which bit of it is meant, and one more
        // step would leave that behind.
        for s in &starts {
            for id in self.simnet_of.get(s).into_iter().flatten() {
                for &h in self.simnet_members.get(id).into_iter().flatten() {
                    if have.insert(h) {
                        group.push((h, true));
                    }
                }
            }
        }
        // The vector a net in hand is a bit of, and that vector's immediate
        // neighbours — the same vector on the far side of its ports. Their
        // statements touch this bit whenever they touch the vector, so both
        // are consulted; their other bits are other nets, so neither is
        // walked.
        let mut consult: Vec<(i64, bool)> = Vec::new();
        for &(h, crossed) in &group {
            for &a in self.vector_of.get(&h).into_iter().flatten() {
                consult.push((a, crossed));
                for &(nb, _) in self.links.get(&a).into_iter().flatten() {
                    consult.push((nb, true));
                }
            }
        }
        for (h, crossed) in consult {
            if have.insert(h) {
                group.push((h, crossed));
            }
        }

        let mut hops = Vec::new();
        let mut seen = HashSet::default();
        let mut seen_bare = HashSet::default();
        for &(handle, crossed) in &group {
            for (inst, local) in self.owners(handle) {
                let Some(&du_handle) = self.du_of_ctx.get(&inst) else { continue };
                let Some(duid) = self.duid_of(du_handle) else { continue };
                let inst_path = self.path_of(inst);

                let (shape_ids, shapeless) = {
                    let m = self.module(duid)?;
                    // A struct or an array is recorded a member at a time, and
                    // a module may record both: `fu_data_i` carries the shapes
                    // that read it whole, `fu_data_i.operation` those that read
                    // that field, and the two sets are different statements. A
                    // question about the object is a question about all of it,
                    // so the members are added to the name rather than used
                    // only when it has nothing of its own — asking about
                    // `fu_data_i` and being told only about the assignments
                    // that take it whole is a missing answer, not a precise
                    // one.
                    let mut names = vec![local.clone()];
                    let mut members: Vec<String> =
                        m.signals.keys().filter(|k| member_of(k, &local)).cloned().collect();
                    members.sort();
                    names.append(&mut members);
                    let mut ids = Vec::new();
                    for n in &names {
                        if let Some((readers, writers)) = m.signals.get(n) {
                            let add = match dir {
                                Direction::Driver => writers,
                                Direction::Load => readers,
                            };
                            for &id in add {
                                if !ids.contains(&id) {
                                    ids.push(id);
                                }
                            }
                        }
                    }
                    // The statement view catches what the shape lists leave
                    // out: a signal with no shape recorded against it is still
                    // read by whatever statement names it.
                    let mut bare: Vec<&str> = Vec::new();
                    for touched in names.iter().filter_map(|n| m.touched.get(n)) {
                        let (reads, writes) = touched;
                        let named = match dir {
                            Direction::Driver => writes,
                            Direction::Load => reads,
                        };
                        for n in named {
                            match m.by_name.get(n).map(std::slice::from_ref).or_else(|| {
                                m.by_tail.get(n).map(Vec::as_slice)
                            }) {
                                Some(found) => {
                                    for &id in found {
                                        if !ids.contains(&id) {
                                            ids.push(id);
                                        }
                                    }
                                }
                                // `shape_tbl` does not hold every statement —
                                // on a large design it holds barely half of
                                // them, and `rw_process_tbl` is the only record
                                // of the rest. It names the file and line, so
                                // the statement can still be reported; dropping
                                // it because no netlist node was kept for it
                                // loses a real answer.
                                None => bare.push(n),
                            }
                        }
                    }
                    if ids.is_empty() && bare.is_empty() {
                        continue;
                    }
                    (ids, bare.into_iter().map(str::to_string).collect::<Vec<_>>())
                };
                for name in shapeless {
                    let m = &self.modules[&duid];
                    let Some(&(file, line)) = m.proc_loc.get(&name) else { continue };
                    // Questa's own statement names carry the block's label in
                    // brackets (`#p#47(label)`) or a second line after a comma
                    // (`#i#125,185`); the identifier test would refuse both and
                    // lose the statement, so a `#tag#` name is judged by its
                    // tag. An implicit wire is the exception: it is elaboration
                    // naming a connection, not a statement anyone wrote, and
                    // reporting it as an endpoint is noise.
                    let tagged = name.split('#').nth(1);
                    if tagged == Some("w") || (tagged.is_none() && !reportable(&name)) {
                        continue;
                    }
                    // The process table names a statement without the block it
                    // sits in, so a generate branch comes back as `#a#86#3`
                    // where the design spells it `gen_lane[3]/#a#86#3`. The
                    // hierarchy has a context for the statement, and its parent
                    // is that block.
                    let scopes: Vec<String> = match self
                        .du_of_ctx
                        .get(&inst)
                        .and_then(|du| self.block_of.get(&(*du, name.clone())))
                    {
                        Some(blocks) => {
                            blocks.iter().map(|b| format!("{inst_path}/{b}")).collect()
                        }
                        None => vec![inst_path.clone()],
                    };
                    for scope in scopes {
                        if !seen_bare.insert((duid, name.clone(), scope.clone())) {
                            continue;
                        }
                        let m = &self.modules[&duid];
                        hops.push(process_hop(m, &name, file, line, &scope, crossed));
                    }
                }
                for sid in shape_ids {
                    let m = &self.modules[&duid];
                    let Some(shape) = m.shapes.get(&sid) else { continue };
                    let Some(stmt) = statement_of(m, sid) else { continue };
                    // Drop an elaboration artefact only when it also points
                    // nowhere. A name that is not an identifier but does carry
                    // a source line is still an answer someone can act on, and
                    // skipping it loses real findings.
                    if !stmt.spec2.is_empty()
                        && !reportable(&stmt.spec2)
                        && stmt.lines.is_empty()
                        && shape.lines.is_empty()
                    {
                        continue;
                    }
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
                        // Keyed by the process table's spelling, which drops the
                        // generate block the shape names.
                        let named = |n: &str| m.proc_loc.get(n).and_then(|(_, l)| *l);
                        named(&stmt.spec2)
                            .or_else(|| stmt.spec2.rsplit_once('/').and_then(|(_, t)| named(t)))
                            .map(Some)
                            .into_iter()
                            .collect()
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
        // What the module tables cannot be asked about. Two things end up
        // here. A variable declared inside a named block gets no `signal_tbl`
        // row, so no name reaches it. And a hierarchical reference —
        // `tb.u_hier_target.deep` read inside a sibling module — is recorded
        // by the reading module under that whole path, which is not a local
        // name of any scope the net sits in, so walking up from the net never
        // arrives at the module doing the reading.
        //
        // The top level records the pair directly, so it answers both. It is
        // consulted always rather than only when nothing else answered: a
        // statement it names is one Questa says touches this net, and the two
        // views of a statement collapse in the pass above.
        {
            let want_writer = matches!(dir, Direction::Driver);
            let touching: Vec<(i64, bool)> = group
                .iter()
                .filter_map(|(h, crossed)| self.proc_of_net.get(h).map(|v| (v, *crossed)))
                .flat_map(|(v, crossed)| {
                    v.iter().filter(|(_, w)| *w == want_writer).map(move |(p, _)| (*p, crossed))
                })
                .collect();
            for (proc, crossed) in touching {
                let Some(c) = self.ctx.get(&proc) else { continue };
                let (name, parent) = (construct_name(&c.name), c.parent);
                let inst_path = self.path_of(parent);
                if !seen_bare.insert((0, name.clone(), inst_path.clone())) {
                    continue;
                }
                let Some(duid) = self.du_of_ctx.get(&parent).and_then(|&h| self.duid_of(h)) else {
                    continue;
                };
                if self.module(duid).is_err() {
                    continue;
                }
                let m = &self.modules[&duid];
                // The tag rewrite covers the constructs it was built from; a
                // design brings others — `#FORCE#`, and the primitive names
                // `#BUF#`/`#AND#` a gate-level netlist is full of. What both
                // spellings share is everything after the tag, so a name the
                // rewrite did not recognise is looked up by that instead of
                // guessed at, and only when it names one statement.
                let Some((name, &(file, line))) = m
                    .proc_loc
                    .get_key_value(&name)
                    .or_else(|| m.proc_by_rest.get(rest_of(&name)?).map(|(k, v)| (k, v)))
                else {
                    continue;
                };
                let name = name.clone();
                hops.push(process_hop(m, &name, file, line, &inst_path, crossed));
            }
        }
        // One statement, one hop. The two tables spell a statement
        // differently — `shape_tbl` gives `#p#379` where `rw_process_tbl`
        // gives `#p#379(exception_handling)` — and a signal reachable both as
        // itself and through a member is found down both paths, so the same
        // assignment can arrive twice under two names. The label is the better
        // name of the two, so the labelled hop is the one kept.
        hops.sort_by_key(|h| std::cmp::Reverse(h.statement.len()));
        let mut kept = HashSet::default();
        hops.retain(|h| {
            kept.insert((
                h.scope.clone(),
                h.file.clone(),
                h.line,
                label_stripped(&h.statement).to_string(),
            ))
        });

        // A port hop says only "the value comes from the other side of this
        // boundary". That is worth reporting when it is all there is — which is
        // what `boundary_only` means — and is noise once the statement itself
        // has been found, since the reader already has the answer it names.
        if hops.iter().any(|h| h.kind != HopKind::Port) {
            hops.retain(|h| h.kind != HopKind::Port);
        }
        hops.sort_by(|a, b| (a.scope.clone(), a.line).cmp(&(b.scope.clone(), b.line)));
        for (i, h) in hops.iter_mut().enumerate() {
            h.group = i + 1;
        }
        Ok(hops)
    }

    fn duid_of(&self, du_handle: i64) -> Option<i64> {
        self.duid_of_handle.get(&du_handle).copied()
    }
}

/// The statement containing `shape`: walk up while the parent is another shape,
/// stopping below the `MODULE` root. A `GATE` names an expression; the
/// `PROCESS` above it names the assignment.
fn statement_of(m: &Module, mut id: i64) -> Option<&schema::Shape> {
    for _ in 0..64 {
        let s = m.shapes.get(&id)?;
        // Stop at the statement rather than at whatever is directly below the
        // module: a generate block sits between the two, and climbing through
        // it returns the block — `gen_counters`, at the line the `for` is on —
        // in place of the `always_ff` inside it. The kind carries a qualifier
        // on some statements (`PROCESS-CAUTION`, `PROCESS-MEMACESS`), which
        // says something about the statement rather than making it not one.
        if s.kind.starts_with("PROCESS") {
            return Some(s);
        }
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

    // The statement's own name, with the block label when the process table
    // has one: `shape_tbl` drops it, and it is how the RTL names the block.
    // A statement inside a generate block is spelled with the branch in
    // front; that prefix is not part of the name the other table keys on, and
    // it does not belong on the scope either — the scope qualifies the
    // operands, and a signal a generate block reads is declared in the module
    // around it, so `uut.genblk3.reg_op1` would name nothing.
    let bare = identifier(&s.spec2);
    let tail = bare.rsplit('/').next().unwrap_or(bare);
    let named = m.labelled.get(tail).map(String::as_str).unwrap_or(bare);

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
    let signals = signals
        .into_iter()
        // A name this module declares is real; so is one reaching into another
        // scope, whose declaration lives there — an interface member is
        // declared in the interface, not in the module using it. What is left
        // is a `vopt` temporary.
        .filter(|n| m.declared.contains(n) || n.contains(['.', '/']))
        .map(|n| join(&scope, &n))
        .collect();

    Hop {
        group: 0,
        kind: kind_of(&s.kind, &s.spec2),
        raw_kind: if s.spec2.is_empty() { s.kind.clone() } else { named.to_string() },
        // The database records a location but no text; the caller fills this in
        // from the source file when it can read it.
        statement: named.to_string(),
        scope,
        file,
        line,
        // A port hop is a boundary by construction, whether or not the walk
        // to it crossed one.
        boundary: crossed || prim.kind == "INST" || s.kind == "INST",
        signals,
    }
}

/// A statement the process table names and the shape table does not hold.
///
/// `shape_tbl` is a netlist, and elaboration keeps a node only for what it
/// needs one for; `rw_process_tbl` is the list of statements, and on a large
/// design it names roughly twice as many. Its row carries the file and the
/// line, which is the answer — there is no primitive to read operands or a
/// clock edge off, so those stay empty rather than being guessed at.
fn process_hop(
    m: &Module,
    name: &str,
    file: i64,
    line: Option<u32>,
    inst_path: &str,
    crossed: bool,
) -> Hop {
    Hop {
        group: 0,
        kind: kind_of("", name),
        raw_kind: name.to_string(),
        statement: name.to_string(),
        scope: inst_path.trim_start_matches('/').replace('/', "."),
        file: m.files.get(file.saturating_sub(1) as usize).filter(|f| !f.is_empty()).cloned(),
        line,
        boundary: crossed,
        signals: Vec::new(),
    }
}

/// A module-local name as a full rwave path. Interface members arrive with
/// their own separator, which becomes rwave's.
fn join(scope: &str, local: &str) -> String {
    let local = local.replace('/', ".");
    if scope.is_empty() { local } else { format!("{scope}.{local}") }
}

/// Sort elaboration's alias rows into edges and vector lookups.
///
/// A scalar alias is one signal under several names, however many rows it
/// has — an init fanned out to thirty-five synchronisers is still one net —
/// and every row stays an edge the walk may pass through. A vector alias
/// names a different net per slice. Rows carrying the same explicit slice
/// are one net and stay edges among themselves; across slices, and across
/// the unranged rows of a vector, which do not say which bit they are, the
/// only relations kept are the two lookups: bits of, and vector of.
///
/// What makes an alias a vector is the simulation-net table (`multibit`:
/// handles seen with two different bit numbers) or a row whose own slice is
/// wider than one bit; the rows alone cannot say, because a vector split
/// bit by bit is written with no ranges at all, exactly like a fanned-out
/// scalar.
#[allow(clippy::type_complexity)]
fn split_aliases(
    rows: Vec<(i64, Option<i64>, Option<i64>, i64)>,
    multibit: &HashSet<i64>,
) -> (Vec<(i64, i64)>, HashMap<i64, Vec<i64>>, HashMap<i64, Vec<i64>>) {
    let mut by_alias: HashMap<i64, Vec<(Option<i64>, Option<i64>, i64)>> = HashMap::default();
    for (a, m, l, f) in rows {
        by_alias.entry(a).or_default().push((m, l, f));
    }
    let mut plain = Vec::new();
    let mut bits: HashMap<i64, Vec<i64>> = HashMap::default();
    let mut of: HashMap<i64, Vec<i64>> = HashMap::default();
    for (a, rows) in by_alias {
        let mut fnets: Vec<i64> = rows.iter().map(|&(_, _, f)| f).collect();
        fnets.sort_unstable();
        fnets.dedup();
        let vector = multibit.contains(&a)
            || rows.iter().any(|&(m, l, _)| matches!((m, l), (Some(m), Some(l)) if m != l));
        if fnets.len() == 1 || !vector {
            for &f in &fnets {
                plain.push((a, f));
            }
            continue;
        }
        // One net per explicit slice: chain its nets, which the walk's
        // transitivity makes a clique of.
        let mut by_slice: HashMap<(i64, i64), Vec<i64>> = HashMap::default();
        for &(m, l, f) in &rows {
            if let (Some(m), Some(l)) = (m, l) {
                by_slice.entry((m, l)).or_default().push(f);
            }
        }
        for (_, mut fs) in by_slice {
            fs.sort_unstable();
            fs.dedup();
            for w in fs.windows(2) {
                plain.push((w[0], w[1]));
            }
        }
        for &f in &fnets {
            of.entry(f).or_default().push(a);
        }
        bits.insert(a, fnets);
    }
    (plain, bits, of)
}

/// A statement's name as the module databases spell it.
///
/// The hierarchy names a statement by its construct — `#ALWAYS#47` — and the
/// module databases by a tag — `#p#47`. Same statement, two spellings, and the
/// location lookup goes through the second.
fn construct_name(name: &str) -> String {
    let mut parts = name.splitn(3, '#');
    let (Some(""), Some(word), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return name.to_string();
    };
    let tag = match word {
        "ALWAYS" => "p",
        "ASSIGN" => "a",
        "INITIAL" => "i",
        "IMPLICIT-WIRE" => "w",
        _ => return name.to_string(),
    };
    format!("#{tag}#{rest}")
}

/// A statement name without the block label Questa appends to it:
/// `#p#379(exception_handling)` and `#p#379` are the same statement, named by
/// the two tables that record it.
fn label_stripped(name: &str) -> &str {
    match name.split_once('(') {
        Some((base, _)) => base,
        None => name,
    }
}

/// What follows a statement name's tag: `399` of `#FORCE#399` and of
/// `#f#399`. The two tables tag the same statement differently and agree on
/// everything after it, which is what lets one be found from the other
/// without a table of tags to keep up to date.
fn rest_of(name: &str) -> Option<&str> {
    let mut parts = name.splitn(3, '#');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(""), Some(_), Some(rest)) if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// Whether `name` is a part of `whole` — `slv_req_i.aw` of `slv_req_i`, or
/// `cpuregs[3]` of `cpuregs`. A longer name that merely starts the same way is
/// a different signal, so the separator has to be there.
fn member_of(name: &str, whole: &str) -> bool {
    name.len() > whole.len()
        && name.starts_with(whole)
        && matches!(name.as_bytes()[whole.len()], b'.' | b'[')
}

/// Whether a shape names something a reader can act on.
///
/// Elaboration produces objects that are not statements and not signals — a
/// memory becomes `_ram (32 X 4096 )`, which is a description rather than a
/// name. Spaces and parentheses are not legal in an identifier, escaped or
/// otherwise, so anything holding them is not something to hand back. Questa's
/// own `#tag#suffix` statement names are legal by construction and pass.
fn reportable(name: &str) -> bool {
    !name.is_empty() && !name.contains([' ', '(', ')', ','])
}

/// The identifier out of a shape's name.
///
/// A memory arrives as `cpuregs (32 X 32 )`: the leading token is the name the
/// RTL declares, and the rest is Questa describing its shape. Nothing legal in
/// an identifier contains a space, so cutting at the first one recovers the
/// name instead of discarding the endpoint.
fn identifier(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
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
            signals: HashMap::default(),
            by_name: HashMap::default(),
            by_tail: HashMap::default(),
            touched: HashMap::default(),
            proc_loc: HashMap::default(),
            labelled: HashMap::default(),
            proc_by_rest: HashMap::default(),
            declared: HashSet::default(),
            files: vec!["dut.sv".into()],
            shapes: HashMap::from_iter([
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
            signals: HashMap::default(),
            by_name: HashMap::default(),
            by_tail: HashMap::default(),
            touched: HashMap::default(),
            proc_loc: HashMap::default(),
            labelled: HashMap::default(),
            proc_by_rest: HashMap::default(),
            declared: HashSet::default(),
            files: vec![],
            shapes: HashMap::from_iter([(1, mk(1, 2)), (2, mk(2, 1))]),
        };
        assert!(statement_of(&m, 1).is_none(), "a malformed tree must not hang");
    }

    #[test]
    fn elaboration_artefacts_that_are_not_names_are_not_reported() {
        // A memory elaborates to a description, not an identifier. Spaces and
        // parentheses cannot appear in a Verilog name of either kind.
        assert!(!reportable("_ram (32 X 4096 )"));
        assert!(!reportable(""));
        assert!(reportable("#p#1402"));
        assert!(reportable("#ALWAYS#251"));
        assert!(reportable("u_alu"));
        assert!(reportable("\\escaped.name"));
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

    #[test]
    fn a_scalar_fanned_out_stays_one_net() {
        // An init driven to three synchronisers under three implicit names:
        // every row unranged and the alias one bit wide. One signal, so all
        // rows stay edges the walk may pass through.
        let rows = vec![(9, None, None, 1), (9, None, None, 2), (9, None, None, 3)];
        let (plain, bits, of) = split_aliases(rows, &HashSet::default());
        assert_eq!(plain.len(), 3);
        assert!(bits.is_empty() && of.is_empty());
    }

    #[test]
    fn a_vectors_bits_are_kept_apart() {
        // A vector split into per-bit nets, rows unranged as Questa writes
        // them: only the simulation-net table says the alias is wide. No
        // edges — a walk through the name would join bit 0 to bit 1.
        let rows = vec![(9, None, None, 1), (9, None, None, 2)];
        let multibit = HashSet::from_iter([9]);
        let (plain, bits, of) = split_aliases(rows, &multibit);
        assert!(plain.is_empty());
        assert_eq!(bits[&9], vec![1, 2]);
        assert_eq!(of[&1], vec![9]);
        assert_eq!(of[&2], vec![9]);
    }

    #[test]
    fn nets_on_the_same_slice_of_a_vector_are_one_net() {
        // A four-bit state crossing a module boundary whole: both sides map
        // to [3:0], so they are one net. The [0:0] tap is not part of it.
        let rows =
            vec![(9, Some(3), Some(0), 1), (9, Some(3), Some(0), 2), (9, Some(0), Some(0), 3)];
        let (plain, bits, _) = split_aliases(rows, &HashSet::default());
        assert_eq!(plain, vec![(1, 2)]);
        assert_eq!(bits[&9], vec![1, 2, 3]);
    }
}
