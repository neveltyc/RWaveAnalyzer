// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `tree`: browse the design hierarchy.
//!
//! Two modes. Without `--of` it prints the scopes below a root — the whole
//! top level by default, or the subtree(s) matched by `--scope` / the `SCOPE`
//! positional. With `--of SIGNAL` it prints that signal's full ancestor chain,
//! top-down, which is the one-shot answer to "I am eight levels deep, what is
//! above me and how do I get back to the DUT top".
//!
//! Everything here is derived from the scope strings the backend already
//! reported, so `tree` works on every format and needs no new backend
//! capability.

use crate::cli::Args;
use crate::filter::SEPARATORS;
use crate::json::{Json, Obj};
use crate::model::Wave;
use crate::select::Selection;

use super::common::*;

/// One hierarchy node. `level` is 0 for a printed root and counts up from there,
/// so it is the indent depth rather than an absolute depth from the file root.
struct Node {
    path: String,
    name: String,
    level: usize,
    /// Signals declared directly in this scope (not in its children).
    signals: usize,
    children: usize,
}

/// Every ancestor prefix of `scope`, shortest first, followed by `scope` itself.
///
/// Slices the original string at separator positions rather than splitting and
/// re-joining, so a `/`-separated FSDB path keeps its slashes and a synthesized
/// prefix is always a real substring of a path the backend actually reported.
fn prefixes(scope: &str) -> impl Iterator<Item = &str> {
    scope
        .char_indices()
        // Skip a separator at index 0: a leading `/` (Questa/VHDL-style paths
        // reach us verbatim through the FSDB backend) would otherwise yield an
        // empty prefix, which then becomes the parent of every top-level scope
        // and collapses the whole tree under one blank-named root.
        .filter(|(i, c)| *i > 0 && SEPARATORS.contains(c))
        .map(move |(i, _)| &scope[..i])
        .chain(std::iter::once(scope))
}

/// The last segment of a scope path (its instance name).
fn last_segment(path: &str) -> &str {
    match path.rfind(SEPARATORS) {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// The parent scope path, or `None` for a top-level scope.
///
/// A separator at index 0 means a leading-separator path such as `/top`, whose
/// parent is the root and not an empty-named scope. Returning `Some("")` there
/// would file every top-level scope under a node that does not exist, leaving
/// the top level empty; `prefixes` guards the same case and the two have to
/// agree.
fn parent_of(path: &str) -> Option<&str> {
    match path.rfind(SEPARATORS) {
        Some(0) | None => None,
        Some(i) => Some(&path[..i]),
    }
}

/// The full scope index: every scope that holds a signal, plus every
/// intermediate scope synthesized from those paths.
///
/// `Wave::scopes()` reports only scopes that directly contain a signal, so a
/// module that holds nothing but sub-modules is missing from it. Walking the
/// prefixes fills those holes; without this the tree would show gaps exactly
/// where the structural hierarchy is most interesting.
struct Index {
    /// Scope path -> directly-declared signal count. Sorted, so iteration and
    /// child lookup are both ordered.
    scopes: std::collections::BTreeMap<String, usize>,
    /// Parent path -> its direct children, in sorted order.
    ///
    /// Materialized once rather than re-derived per node: scanning every scope
    /// to find one node's children turns a walk into O(N²), which on a 56k-scope
    /// design is tens of seconds — and `--limit` cannot save it, because the
    /// walk has to finish before there is a total to clip.
    children: std::collections::BTreeMap<String, Vec<String>>,
    /// Top-level scopes (those with no parent), in sorted order.
    tops: Vec<String>,
    /// Signals sitting at the file root (empty scope).
    root_signals: usize,
}

impl Index {
    fn build(wave: &Wave) -> Index {
        let mut scopes: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut root_signals = 0usize;
        for i in 0..wave.signal_count() {
            for (_, scope) in wave.signal(i).alias_pairs() {
                if scope.is_empty() {
                    root_signals += 1;
                    continue;
                }
                // Materialize the ancestors so intermediate scopes exist even
                // when they hold no signals of their own. Only on first sight
                // of a scope: the prefixes are the same for every alias in it.
                let seen = scopes.contains_key(scope);
                *scopes.entry(scope.to_string()).or_insert(0) += 1;
                if !seen {
                    for p in prefixes(scope) {
                        scopes.entry(p.to_string()).or_insert(0);
                    }
                }
            }
        }
        // One pass to invert the scope set into a parent -> children map. The
        // BTreeMap is already sorted, so each child list comes out sorted too.
        let mut children: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut tops: Vec<String> = Vec::new();
        for path in scopes.keys() {
            match parent_of(path) {
                Some(p) => children.entry(p.to_string()).or_default().push(path.clone()),
                None => tops.push(path.clone()),
            }
        }
        Index { scopes, children, tops, root_signals }
    }

    /// Direct children of `parent`, in sorted order.
    fn children_of(&self, parent: &str) -> &[String] {
        self.children.get(parent).map_or(&[], Vec::as_slice)
    }

    fn child_count(&self, path: &str) -> usize {
        self.children_of(path).len()
    }

    fn signals(&self, path: &str) -> usize {
        self.scopes.get(path).copied().unwrap_or(0)
    }

    /// Append `path` and its descendants, stopping `max_level` levels below the
    /// root. `level` is the indent depth of `path` itself, so `max_level == 1`
    /// yields the root and its direct children, matching what `--depth 1` means
    /// for `list`: one level below the matched scope.
    fn walk(&self, path: &str, level: usize, max_level: usize, out: &mut Vec<Node>) {
        out.push(Node {
            path: path.to_string(),
            name: last_segment(path).to_string(),
            level,
            signals: self.signals(path),
            children: self.child_count(path),
        });
        if level >= max_level {
            return;
        }
        // `walk` takes &self and children_of hands back a borrow of the same
        // map, so recursing while iterating is two shared borrows.
        for c in self.children_of(path) {
            self.walk(c, level + 1, max_level, out);
        }
    }
}

/// The starting scope: the `SCOPE` positional and `--scope` mean the same thing;
/// the positional wins when both are given.
fn root_pattern(args: &Args) -> Option<String> {
    let pick = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    pick(&args.target).or_else(|| pick(&args.scope))
}

/// Resolve the roots to print. With no pattern that is the top level; with one
/// it is every scope the pattern matches whose parent does *not* match, so a
/// subtree is rooted where the match begins rather than repeated at every
/// descendant.
fn roots(index: &Index, args: &Args) -> Result<Vec<String>, String> {
    let pattern = match root_pattern(args) {
        None => return Ok(index.tops.clone()),
        Some(p) => p,
    };
    // Reuse the documented `--scope` pattern language rather than re-implementing
    // it: bare name = instance at any level, dotted = segment-aligned suffix.
    let sel = Selection::parse(&Some(pattern.clone()), None, &None, &None)?;
    let matched = |s: &str| sel.keeps_alias("", s);
    let out: Vec<String> = index
        .scopes
        .keys()
        .filter(|s| matched(s) && !parent_of(s).is_some_and(matched))
        .cloned()
        .collect();
    if out.is_empty() {
        return Err(format!(
            "no scope matches {}; run tree without a scope to see the top level",
            crate::format::pyrepr(&pattern)
        ));
    }
    Ok(out)
}

/// Shared computation: the node list plus the header facts both renderers need.
struct TreeData {
    /// `Some` in `--of` mode: the resolved signal path.
    of: Option<String>,
    /// The scope path(s) the listing is rooted at. Several when a pattern
    /// matched the same instance name in more than one place.
    roots: Vec<String>,
    depth: usize,
    nodes: Vec<Node>,
    total: usize,
    shown: usize,
    truncated: bool,
    root_signals: usize,
}

fn build(wave: &mut Wave, args: &Args) -> Result<TreeData, String> {
    let index = Index::build(wave);
    // `--depth` counts levels below the root, same as everywhere else; `tree`
    // just counts scopes where `list` counts signals.
    let depth = args.depth.unwrap_or(1).max(1) as usize;

    let (of, roots_v, mut nodes) = if let Some(pat) =
        args.of.as_deref().filter(|s| !s.trim().is_empty())
    {
        let (path, scope) = resolve_signal_path(wave, pat, "--of")?;
        // The chain is every ancestor of the signal's own scope, top-down.
        let mut nodes = Vec::new();
        if !scope.is_empty() {
            for (level, p) in prefixes(&scope).enumerate() {
                nodes.push(Node {
                    path: p.to_string(),
                    name: last_segment(p).to_string(),
                    level,
                    signals: index.signals(p),
                    children: index.child_count(p),
                });
            }
        }
        let chain_root = if scope.is_empty() { Vec::new() } else { vec![scope] };
        (Some(path), chain_root, nodes)
    } else {
        let rs = roots(&index, args)?;
        let mut nodes = Vec::new();
        for r in &rs {
            index.walk(r, 0, depth, &mut nodes);
        }
        (None, rs, nodes)
    };

    let total = nodes.len();
    let limit = limit_of(args);
    let (shown, truncated) = clip_len(total, limit);
    nodes.truncate(shown);
    Ok(TreeData {
        of,
        roots: roots_v,
        depth,
        nodes,
        total,
        shown,
        truncated,
        root_signals: index.root_signals,
    })
}

pub(super) fn compute_tree(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let d = build(wave, args)?;
    let mut o = Obj::new();
    match &d.of {
        Some(sig) => {
            o = o
                .push("mode", Json::str("chain"))
                .push("signal", Json::str(sig.clone()));
        }
        None => {
            // An array, not a display string: a pattern can legitimately match
            // the same instance name in several places, and a consumer needs
            // the paths rather than a count to act on.
            o = o
                .push("mode", Json::str("subtree"))
                .push(
                    "roots",
                    Json::Array(d.roots.iter().map(|r| Json::str(r.clone())).collect()),
                )
                .push("depth", Json::Int(d.depth as i64));
        }
    }
    if d.of.is_none() {
        o = o.push("root_signals", Json::Int(d.root_signals as i64));
    }
    o = o
        .push("total", Json::Int(d.total as i64))
        .push("shown", Json::Int(d.shown as i64))
        .push("truncated", Json::Bool(d.truncated));
    o = push_trunc_hint(o, d.truncated, d.shown, d.total, true, "scopes");
    let rows: Vec<Json> = d
        .nodes
        .iter()
        .map(|n| {
            Obj::new()
                .push("path", Json::str(n.path.clone()))
                .push("name", Json::str(n.name.clone()))
                .push("level", Json::Int(n.level as i64))
                .push("signals", Json::Int(n.signals as i64))
                .push("children", Json::Int(n.children as i64))
                .build()
        })
        .collect();
    let key = if d.of.is_some() { "chain" } else { "scopes" };
    Ok(o.push(key, Json::Array(rows)).build())
}

pub(super) fn text_tree(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let d = build(wave, args)?;
    match &d.of {
        Some(sig) => println!("{sig} — ancestors (top-down)"),
        None => {
            let where_ = match d.roots.len() {
                0 => "(nothing)".to_string(),
                1 => d.roots[0].clone(),
                n => format!("{n} matching scopes"),
            };
            println!("{where_} — {} scope(s), depth {}", d.total, d.depth);
        }
    }
    if d.nodes.is_empty() {
        println!();
        if d.of.is_some() {
            println!("  (top-level signal; no enclosing scope)");
        } else {
            println!("  (no scopes)");
            // A flat design has signals but no hierarchy at all — say so here,
            // because the usual place this is reported is below the node list
            // that does not exist.
            if d.root_signals > 0 {
                println!("  ({} signal(s) at the file root)", d.root_signals);
            }
        }
        return Ok(());
    }
    println!();
    // `--of` prints full paths so a line can be pasted straight into --scope.
    // Subtree mode indents instance names to show the shape, but spells each
    // *root* out in full: with a pattern like `--scope u_a` the roots are
    // several different instances that share a name, and printing just the
    // name would render them as identical rows.
    let label = |n: &Node| -> String {
        if d.of.is_some() || n.level == 0 {
            n.path.clone()
        } else {
            format!("{}{}", "  ".repeat(n.level), n.name)
        }
    };
    let width = d.nodes.iter().map(|n| label(n).chars().count()).max().unwrap_or(0);
    for n in &d.nodes {
        let mut extra = format!("{} signal(s)", n.signals);
        if n.children > 0 {
            extra.push_str(&format!(", {} child scope(s)", n.children));
        }
        println!("  {}  {}", ljust(&label(n), width), extra);
    }
    if d.root_signals > 0 && d.of.is_none() {
        println!("\n  ({} signal(s) at the file root, outside any scope)", d.root_signals);
    }
    if d.truncated {
        println!("{}", trunc_line(d.shown, d.total, "scopes"));
    }
    Ok(())
}
