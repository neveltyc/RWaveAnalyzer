// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `search` command: evaluate conditions over time — interval mode (spans
//! where conditions hold), segment mode (intervals split by `--show` value
//! changes), and event mode (any clause carries a `changed(SIG)` term: fire at
//! the ticks where SIG transitions while the rest of the clause holds).

use std::collections::{BTreeMap, BTreeSet};
use crate::cli::Args;
use crate::condition::{self, Op, ParsedCondition, TermBody};
use crate::filter::{Filters, MatchMode};
use crate::format::{fmt_time, fmt_val, parse_time, TimeParseError, ValueKind};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use crate::select::Selection;
use super::common::*;

/// A resolved condition: a parsed term bound to a specific signal id.
struct ResolvedCond {
    sid: Sid,
    term: ResolvedTerm,
    original: String,
    path: String,
}

/// The signal-resolved body of one condition term.
enum ResolvedTerm {
    /// `SIG op VAL` — evaluated against the current state.
    Level {
        op: Op,
        target: condition::Target,
        width: u32,
        /// Formatting class of the bound signal; decides whether the recorded
        /// value is treated as a logic-bit string during matching (see
        /// `conditions_hold`).
        kind: ValueKind,
        value_text: String,
    },
    /// `changed(SIG)` — evaluated against the set of signals that transitioned
    /// at the tick under evaluation (event mode only; setup rejects it in
    /// interval/segment mode by requiring every clause to carry one or none).
    Changed,
}

/// How the selection options bear on a name lookup, for the error text. Empty
/// when nothing is narrowing, so an unconstrained failure reads as it always
/// did.
fn within_selection(sel: &Selection) -> String {
    let gates = sel.active_gates();
    if gates.is_empty() {
        String::new()
    } else {
        format!(" within the current selection ({gates})")
    }
}

/// Resolve a single signal pattern to exactly one sid. An exact full-path match
/// (case-insensitive) wins over substring matches; otherwise fall back to the
/// normal filter matcher, restricted to `sel`, and require a unique result.
///
/// Only the fallback is restricted. A path spelled out in full is an explicit
/// choice, so a broad `--exclude` — or one inherited from a `--batch` line —
/// must not make a named signal unreachable.
fn resolve_one_signal(
    wave: &Wave,
    sel: &Selection,
    pattern: &str,
    role: &str,
    mode: MatchMode,
) -> Result<Sid, String> {
    let pat = pattern.trim();
    let pl = pat.to_lowercase();
    let has_wild = pat.contains('*') || pat.contains('?');

    if !has_wild {
        let exact = sids_where(wave, |info| info.has_exact_path_ci(&pl));
        if exact.len() == 1 {
            return Ok(exact[0]);
        }
        if exact.len() > 1 {
            let examples = example_paths(wave, &exact);
            return Err(format!(
                "{role} pattern {} exactly matches {} signals; use list to choose a more specific name, examples: {}", crate::format::pyrepr(pattern),
                exact.len(),
                examples
            ));
        }
    }

    // Fall back to filter matching, inside the current selection.
    let filters = Filters::parse_mode(&[pat], mode).map_err(|e| e.0)?;
    let matched = sids_where(wave, |info| sel.keeps_signal_matching(info, &filters));
    if matched.is_empty() {
        let scoped = within_selection(sel);
        let hint = if scoped.is_empty() {
            String::new()
        } else {
            "; give the full hierarchical path to look outside it".to_string()
        };
        return Err(format!(
            "{role} pattern {} matches no signals{scoped}{hint}",
            crate::format::pyrepr(pattern)
        ));
    }
    if matched.len() != 1 {
        let examples = example_paths(wave, &matched);
        let extra = if examples.is_empty() {
            String::new()
        } else {
            format!(", examples: {examples}")
        };
        return Err(format!(
            "{role} pattern {} matches {} signals{}; narrow it with --scope or --exclude, or give the full hierarchical path{extra}",
            crate::format::pyrepr(pattern),
            matched.len(),
            within_selection(sel),
        ));
    }
    Ok(matched[0])
}

fn example_paths(wave: &Wave, sids: &[Sid]) -> String {
    let mut paths: Vec<String> = sids.iter().map(|s| wave.signal(*s).path.clone()).collect();
    paths.sort();
    paths.truncate(5);
    paths.join(", ")
}

/// Resolve `--show` patterns to a sorted, de-duplicated set of sids. Exact
/// full-path match wins per-pattern and bypasses the selection, as in
/// [`resolve_one_signal`]; otherwise pattern matching applies within `sel`.
fn resolve_show_sids(
    wave: &Wave,
    sel: &Selection,
    show: &Option<String>,
    mode: MatchMode,
) -> Result<Vec<Sid>, String> {
    let raw = match show {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let pats: Vec<&str> = raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if pats.is_empty() {
        return Ok(Vec::new());
    }
    let mut selected: BTreeSet<Sid> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();
    for pat in pats {
        let has_wild = pat.contains('*') || pat.contains('?');
        if !has_wild {
            let pl = pat.to_lowercase();
            let exact = sids_where(wave, |info| info.has_exact_path_ci(&pl));
            if !exact.is_empty() {
                selected.extend(exact);
                continue;
            }
        }
        let filters = Filters::parse_mode(&[pat], mode).map_err(|e| e.0)?;
        let matched = sids_where(wave, |info| sel.keeps_signal_matching(info, &filters));
        if matched.is_empty() {
            missing.push(pat.to_string());
        } else {
            selected.extend(matched);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "--show matches no signals{}: {}",
            within_selection(sel),
            missing.join(", ")
        ));
    }
    if selected.is_empty() {
        return Err("--show matches no signals".to_string());
    }
    let mut out: Vec<Sid> = selected.into_iter().collect();
    out.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));
    Ok(out)
}

/// Resolve the comma-separated condition string against the waveform.
fn resolve_conditions(
    wave: &Wave,
    sel: &Selection,
    text: &str,
    mode: MatchMode,
) -> Result<Vec<ResolvedCond>, String> {
    let parsed: Vec<ParsedCondition> = condition::parse_conditions(text).map_err(|e| e.0)?;
    let mut resolved: Vec<ResolvedCond> = Vec::new();
    let mut seen: BTreeSet<(Sid, &'static str, String)> = BTreeSet::new();
    for c in parsed {
        let role = match c.term {
            TermBody::Changed => "changed() signal",
            TermBody::Level { .. } => "condition signal",
        };
        let sid = resolve_one_signal(wave, sel, &c.pattern, role, mode)?;
        let info = wave.signal(sid);
        let term = match c.term {
            TermBody::Level { op, target, value_text } => ResolvedTerm::Level {
                op,
                target,
                width: info.width,
                kind: info.kind,
                value_text,
            },
            TermBody::Changed => ResolvedTerm::Changed,
        };
        let cond = ResolvedCond { sid, term, original: c.original, path: info.path.clone() };
        if seen.insert(term_key(&cond)) {
            resolved.push(cond);
        }
    }
    Ok(resolved)
}

/// De-duplication key for one resolved term: resolved `sid` (folds alias paths
/// to one signal), the operator slot (`changed` for edge predicates, which no
/// comparison operator can spell), and the target value *as written* (`5` and
/// `0x5` differ → not folded; no cross-base normalizing). Shared by the
/// within-clause term de-dup and `clause_key` so the two can never drift apart.
fn term_key(c: &ResolvedCond) -> (Sid, &'static str, String) {
    match &c.term {
        ResolvedTerm::Level { op, target, .. } => (c.sid, op.as_str(), target.dedup_key()),
        ResolvedTerm::Changed => (c.sid, "changed", String::new()),
    }
}

/// Evaluate whether all terms of one clause hold. `state` maps sid to the raw
/// decoded value string (absent => undefined); `changed` is the set of signals
/// that truly transitioned at the tick under evaluation (empty outside event
/// mode).
fn conditions_hold(
    state: &BTreeMap<Sid, String>,
    changed: &BTreeSet<Sid>,
    conds: &[ResolvedCond],
) -> bool {
    for c in conds {
        match &c.term {
            ResolvedTerm::Changed => {
                if !changed.contains(&c.sid) {
                    return false;
                }
            }
            ResolvedTerm::Level { op, target, width, kind, .. } => {
                let raw = state.get(&c.sid).map(|s| s.as_str());
                // Classify the recorded value as a logic-bit string by the
                // signal's declared `kind`, never by sniffing its characters. A
                // real signal renders as decimal text (e.g. 100.0 -> "100");
                // treating that as a bit string made `dac=4` spuriously match
                // (binary 100 == 4) and `dac=100` miss. Non-logic signals
                // (real/string/event) carry `None` here and so never satisfy a
                // numeric/bit target — only the literal-compare path.
                let bits = match kind {
                    ValueKind::Bits => raw,
                    _ => None,
                };
                if !condition::condition_match(bits, raw, *op, target, *width) {
                    return false;
                }
            }
        }
    }
    true
}

/// OR across clauses: the search holds at a time when *any* clause's AND-terms
/// all hold. With a single clause this is exactly `conditions_hold`, so single
/// `--condition` behavior is unchanged.
fn any_clause_holds(
    state: &BTreeMap<Sid, String>,
    changed: &BTreeSet<Sid>,
    clauses: &[Vec<ResolvedCond>],
) -> bool {
    clauses.iter().any(|c| conditions_hold(state, changed, c))
}

/// Order-independent canonical key for clause de-duplication (PRD §7). Reuses
/// the per-term key shape from `resolve_conditions` (see [`term_key`]).
fn clause_key(clause: &[ResolvedCond]) -> Vec<(Sid, &'static str, String)> {
    let mut key: Vec<(Sid, &'static str, String)> = clause.iter().map(term_key).collect();
    key.sort();
    key
}

/// Render one clause's raw label: its original `SIG op VAL` terms joined by `,`.
fn condition_label(conds: &[ResolvedCond]) -> String {
    conds.iter().map(|c| c.original.clone()).collect::<Vec<_>>().join(",")
}

/// Render one clause's resolved label: `path op value` / `changed(path)` terms
/// joined by `,`.
fn condition_result_text(conds: &[ResolvedCond]) -> String {
    conds
        .iter()
        .map(|c| match &c.term {
            ResolvedTerm::Level { op, value_text, .. } => {
                format!("{}{}{}", c.path, op.as_str(), value_text)
            }
            ResolvedTerm::Changed => format!("changed({})", c.path),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Join per-clause labels for echo (PRD §4): a lone clause renders as-is (no
/// parens, preserving single-`--condition` output byte-for-byte); multiple
/// clauses are each parenthesized and joined with ` OR `.
fn join_clauses(
    clauses: &[Vec<ResolvedCond>],
    render: impl Fn(&[ResolvedCond]) -> String,
) -> String {
    if clauses.len() == 1 {
        render(&clauses[0])
    } else {
        clauses
            .iter()
            .map(|c| format!("({})", render(c)))
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}

/// Build the ordered (path-sorted, by show_sids order) show-value map for the
/// current state. Returns a Vec of (path, value) preserving show_sids order.
fn show_values(
    wave: &Wave,
    state: &BTreeMap<Sid, String>,
    show_sids: &[Sid],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for &sid in show_sids {
        let info = wave.signal(sid);
        let raw = state.get(&sid);
        let v = match raw {
            Some(r) => fmt_val(r, info.kind, info.width),
            None => "(undef)".to_string(),
        };
        out.push((info.path.clone(), v));
    }
    out
}

fn values_text(values: &[(String, String)]) -> String {
    values
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn values_json(values: &[(String, String)]) -> Json {
    let members: Vec<(String, Json)> = values
        .iter()
        .map(|(k, v)| (k.clone(), Json::str(v.clone())))
        .collect();
    Json::Object(members)
}

/// Build the verbose `meta` object for show signals: `{path: {raw, width,
/// type}}`. `raw` is the raw decoded value string (bit string for logic), or
/// JSON null when the signal is undefined in the current state.
fn show_meta(wave: &Wave, state: &BTreeMap<Sid, String>, show_sids: &[Sid]) -> Json {
    let mut members: Vec<(String, Json)> = Vec::new();
    for &sid in show_sids {
        let info = wave.signal(sid);
        let raw = state.get(&sid);
        let raw_json = match raw {
            Some(r) => Json::str(r.clone()),
            None => Json::Null,
        };
        let entry = Obj::new()
            .push("raw", raw_json)
            .push("width", Json::Int(info.width as i64))
            .push("type", Json::str(info.type_str))
            .build();
        members.push((info.path.clone(), entry));
    }
    Json::Object(members)
}

/// Resolve the search end time: explicit `--end`, else the file's max tick.
/// Why a search came back with nothing. The text renderers already said this;
/// a JSON caller was left with an empty array and no reason at all.
fn no_match_reason(s: &SearchSetup) -> String {
    format!(
        "the condition {} never held in {}..{}",
        s.cond_text,
        fmt_time(s.t0, s.ts),
        fmt_time(s.t1, s.ts)
    )
}

fn search_end_time(wave: &Wave, t1: Option<i64>) -> Result<i64, String> {
    if let Some(t1) = t1 {
        return Ok(t1);
    }
    match wave.time_range() {
        Some((_mn, mx)) => Ok(mx),
        None => Err(
            "search cannot evaluate condition: VCD data section contains no value changes"
                .to_string(),
        ),
    }
}

/// One fired `search` event (event mode).
struct Ev {
    time_ticks: i64,
    time_h: String,
    values: Vec<(String, String)>,
    meta: Option<Json>,
}

/// One emitted interval/segment (interval & segment modes).
#[derive(Clone)]
struct IntervalRow {
    begin_ticks: i64,
    end_ticks: i64,
    values: Option<Vec<(String, String)>>,
    meta: Option<Json>,
}

/// Resolved inputs shared by the `search` collectors and renderers: the parsed
/// condition clauses, the `--show` selection, the loaded signal set, the time
/// window, and the display labels. Built once per invocation.
struct SearchSetup {
    /// OR clauses: each inner vec is one `--condition` (a comma-separated AND
    /// list); a time satisfies the search when *any* clause holds.
    clauses: Vec<Vec<ResolvedCond>>,
    show_sids: Vec<Sid>,
    /// Distinct signals under a `changed()` term, path-sorted. Non-empty ⇔
    /// event mode (setup guarantees every clause carries a changed() term
    /// then); empty ⇔ interval/segment mode.
    changed_sids: Vec<Sid>,
    sel_ref: Vec<Sid>,
    t0: i64,
    t1: i64,
    limit: usize,
    verbose: bool,
    cond_label: String,
    cond_text: String,
    ts: f64,
}

fn search_setup(wave: &mut Wave, args: &Args) -> Result<SearchSetup, String> {
    let ts = wave.ts_sec();
    let t0 = match &args.begin {
        Some(b) => parse_time(b, ts).map_err(|e: TimeParseError| e.0)?,
        None => 0,
    };
    let t1_raw = match &args.end {
        Some(e) => Some(parse_time(e, ts).map_err(|e: TimeParseError| e.0)?),
        None => None,
    };
    let t1 = search_end_time(wave, t1_raw)?;
    if t1 < t0 {
        // Without an explicit `--end` the bound is the trace's own last event,
        // so blaming `--end` would name a flag the user never wrote.
        return Err(match &args.end {
            Some(_) => "end time must be >= begin time".to_string(),
            None => format!(
                "--begin {} is after the last event at {}; the window is empty",
                fmt_time(t0, ts),
                fmt_time(t1, ts)
            ),
        });
    }

    if args.condition.is_empty() {
        return Err("the following arguments are required: --condition".into());
    }
    // `search` selects through its condition and `--show` names rather than a
    // row filter, so the selection options apply to *resolving* those names:
    // they are the context a bare name is looked up in, which is usually what
    // turns "matches N signals" into a unique hit. A name written as a full
    // path is exempt.
    let mode = match_mode(args);
    let sel = selection_of(args)?;
    // Each `--condition` is one OR clause (a comma-separated AND list). Resolve
    // every clause, then drop duplicate clauses (PRD §7): identical, term-order-
    // permuted, or alias-equivalent clauses fold to one; different value
    // spellings (`5` vs `0x5`) stay distinct. First occurrence wins, order kept;
    // de-dup is silent. Per-clause parse/resolution errors surface verbatim.
    let mut clauses: Vec<Vec<ResolvedCond>> = Vec::new();
    let mut seen: BTreeSet<Vec<(Sid, &'static str, String)>> = BTreeSet::new();
    for text in &args.condition {
        let clause = resolve_conditions(wave, &sel, text, mode)?;
        if seen.insert(clause_key(&clause)) {
            clauses.push(clause);
        }
    }

    // Mode split: a clause with a changed() term describes ticks (event mode);
    // a clause without one describes spans (interval/segment mode). The two
    // row shapes cannot merge, so mixing is rejected: either every clause
    // carries a changed() term or none does.
    let with_changed = clauses
        .iter()
        .filter(|cl| cl.iter().any(|c| matches!(c.term, ResolvedTerm::Changed)))
        .count();
    if with_changed > 0 && with_changed < clauses.len() {
        return Err(
            "cannot mix changed() and level-only --condition clauses: a changed() clause \
             fires at ticks (event mode) while a level-only clause spans time (interval \
             mode); give every clause a changed() term, or run two searches"
                .to_string(),
        );
    }
    let changed_set: BTreeSet<Sid> = clauses
        .iter()
        .flatten()
        .filter(|c| matches!(c.term, ResolvedTerm::Changed))
        .map(|c| c.sid)
        .collect();
    let mut changed_sids: Vec<Sid> = changed_set.into_iter().collect();
    changed_sids.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));

    let mut show_sids = resolve_show_sids(wave, &sel, &args.show, mode)?;
    if !changed_sids.is_empty() && show_sids.is_empty() {
        // Event mode with no --show: default to watching the changed() signals.
        show_sids = changed_sids.clone();
    }

    // The set of signals we must load: the union of every clause's signals
    // (changed() terms included) + show. Cost scales with the distinct signals
    // referenced, not the clause count (PRD §9).
    let mut selected: BTreeSet<Sid> = clauses.iter().flatten().map(|c| c.sid).collect();
    selected.extend(show_sids.iter().copied());
    let sel_vec: Vec<Sid> = selected.iter().copied().collect();
    wave.ensure_loaded(&sel_vec);

    let cond_label = join_clauses(&clauses, condition_label);
    let cond_text = join_clauses(&clauses, condition_result_text);

    Ok(SearchSetup {
        clauses,
        show_sids,
        changed_sids,
        sel_ref: sel_vec,
        t0,
        t1,
        limit: limit_of(args),
        verbose: args.verbose,
        cond_label,
        cond_text,
        ts,
    })
}

pub(super) fn compute_search(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let s = search_setup(wave, args)?;
    if !s.changed_sids.is_empty() {
        let (events, total, truncated) = search_event_collect(wave, &s);
        Ok(search_event_json(wave, &s, &events, total, truncated))
    } else {
        let (results, total, truncated, has_show) = search_interval_collect(wave, &s);
        Ok(search_interval_json(wave, &s, &results, total, truncated, has_show))
    }
}

pub(super) fn text_search(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let s = search_setup(wave, args)?;
    if !s.changed_sids.is_empty() {
        let (events, total, truncated) = search_event_collect(wave, &s);
        search_event_text(&s, &events, total, truncated);
    } else {
        let (results, total, truncated, has_show) = search_interval_collect(wave, &s);
        search_interval_text(&s, &results, total, truncated, has_show);
    }
    Ok(())
}

/// Event mode collect: fire at ticks where any clause holds — its changed()
/// signals all truly transition at that tick and its level terms hold. Groups
/// events by timestamp; a t=0 initialization is not a change. Returns
/// `(events, total, truncated)`.
fn search_event_collect(
    wave: &Wave,
    s: &SearchSetup,
) -> (Vec<Ev>, usize, bool) {
    let sel: &[Sid] = &s.sel_ref;
    let clauses: &[Vec<ResolvedCond>] = &s.clauses;
    let show_sids: &[Sid] = &s.show_sids;
    let t0 = s.t0;
    let t1 = s.t1;
    let limit = s.limit;
    let verbose = s.verbose;
    let ts = s.ts;

    let mut state: BTreeMap<Sid, String> = BTreeMap::new();
    let mut events: Vec<Ev> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut cur_t: Option<i64> = None;
    let mut group: Vec<(Sid, String)> = Vec::new();

    // We need to process completed groups. Because for_each_event is a closure
    // callback, collect (t, sid, raw) into a buffer first for clarity. Files
    // this tool targets fit comfortably in memory; this keeps the state machine
    // straightforward without fighting the borrow checker.
    let mut stream: Vec<(i64, Sid, String)> = Vec::new();
    wave.for_each_event(0, Some(t1), Some(sel), |t, sid, val| {
        stream.push((t, sid, val.raw_str().into_owned()));
    });

    let process_group =
        |state: &mut BTreeMap<Sid, String>, group: &[(Sid, String)], gt: i64| -> bool {
            // Compute the set of signals that truly transitioned at gt, apply
            // the group, then evaluate the clauses post-update: fired ⇔ any
            // clause's changed() signals are all in the set and its level
            // terms hold.
            let mut changed: BTreeSet<Sid> = BTreeSet::new();
            for (gsid, gval) in group {
                let old = state.get(gsid);
                let is_event = wave.signal(*gsid).kind == ValueKind::Event;
                if gt == 0 && old.is_none() {
                    // initialization, not a change
                } else if is_event {
                    changed.insert(*gsid);
                } else if old.is_none() {
                    // first definition, not a change
                } else if old.map(|s| s.as_str()) != Some(gval.as_str()) {
                    changed.insert(*gsid);
                }
            }
            for (gsid, gval) in group {
                state.insert(*gsid, gval.clone());
            }
            any_clause_holds(state, &changed, clauses)
        };

    'outer: for (t, sid, raw) in stream {
        // Edge semantics: event mode reports value *changes* within [t0, t1], so
        // a change landing exactly at t0 is inside the window and must be
        // processed (it can fire an event). Only strictly-earlier changes form
        // the baseline. This `< t0` deliberately differs from interval mode's
        // `<= t0` (level semantics) below — see the note there; do not unify.
        if t < t0 {
            state.insert(sid, raw);
            continue;
        }
        if cur_t.is_none() {
            cur_t = Some(t);
        }
        if Some(t) != cur_t {
            let gt = cur_t.unwrap();
            let fired = process_group(&mut state, &group, gt);
            if fired {
                total += 1;
                if limit != 0 && events.len() >= limit {
                    truncated = true;
                    break 'outer;
                }
                let values = show_values(wave, &state, show_sids);

                let meta = if verbose { Some(show_meta(wave, &state, show_sids)) } else { None };
                events.push(Ev {
                    time_ticks: gt,
                    time_h: fmt_time(gt, ts),
                    values,
                    meta,
                });
            }
            cur_t = Some(t);
            group = Vec::new();
        }
        group.push((sid, raw));
    }
    // Final pending group.
    if !group.is_empty() && !truncated {
        let gt = cur_t.unwrap();
        let fired = process_group(&mut state, &group, gt);
        if fired {
            total += 1;
            if limit != 0 && events.len() >= limit {
                truncated = true;
            } else {
                let values = show_values(wave, &state, show_sids);

                let meta = if verbose { Some(show_meta(wave, &state, show_sids)) } else { None };
                events.push(Ev {
                    time_ticks: gt,
                    time_h: fmt_time(gt, ts),
                    values,
                    meta,
                });
            }
        }
    }

    (events, total, truncated)
}

fn search_event_json(
    wave: &Wave,
    s: &SearchSetup,
    events: &[Ev],
    total: usize,
    truncated: bool,
) -> Json {
    // The same row shape all three modes emit. An event fires at an instant,
    // so it fills `begin_*` and leaves `end_*` null; an interval or segment
    // spans one. Sharing the shape is what lets the three modes share a key.
    let evs: Vec<Json> = events
        .iter()
        .map(|e| {
            let mut o = Obj::new()
                .push("begin_ticks", Json::Int(e.time_ticks))
                .push("begin_h", Json::str(e.time_h.clone()))
                .push("end_ticks", Json::Null)
                .push("end_h", Json::Null)
                .push("values", values_json(&e.values));
            if s.verbose {
                o = o.push("meta", e.meta.clone().unwrap_or_else(|| Json::Object(Vec::new())));
            }
            o.build()
        })
        .collect();
    let show_paths: Vec<Json> = s
        .show_sids
        .iter()
        .map(|sid| Json::str(wave.signal(*sid).path.clone()))
        .collect();
    let (total_field, trunc_final) = if truncated {
        (events.len() + 1, true)
    } else {
        (total, false)
    };
    let changed_paths: Vec<Json> = s
        .changed_sids
        .iter()
        .map(|sid| Json::str(wave.signal(*sid).path.clone()))
        .collect();
    let obj = Obj::new()
        .push("mode", Json::str("event"))
        .push("condition", Json::str(s.cond_label.clone()))
        .push("condition_resolved", Json::str(s.cond_text.clone()))
        .push("changed", Json::Array(changed_paths))
        .push("show", Json::Array(show_paths))
        .push("window", window_json(s.t0, Some(s.t1), s.ts));
    let obj = push_counts(obj, events.len(), total_field, !trunc_final, trunc_final);
    let mut hints = Hints::new();
    hints.push_opt(trunc_hint(trunc_final, events.len(), total_field, false, "rows"));
    if events.is_empty() {
        hints.push(no_match_reason(s));
    }
    hints.attach(obj).push("rows", Json::Array(evs)).build()
}

fn search_event_text(
    s: &SearchSetup,
    events: &[Ev],
    total: usize,
    truncated: bool,
) {
    if !events.is_empty() {
        println!(
            "Found: {} event(s)",
            count_label(if truncated { events.len() + 1 } else { total }, truncated)
        );
        for e in events {
            println!("  T={} {}", ljust(&e.time_h, 12), values_text(&e.values));
        }
        if truncated {
            println!("{}", trunc_line_lb(events.len(), events.len() + 1, "events"));
        }
    } else {
        println!(
            "No event in {}..{} where {}.",
            fmt_time(s.t0, s.ts),
            fmt_time(s.t1, s.ts),
            s.cond_text
        );
    }
}

/// Interval mode (no `--show`): emit `[a, b)` intervals where conditions hold.
/// Segment mode (`--show` present): an interval further split whenever the
/// displayed show-value tuple changes while the condition remains true.
/// Returns `(results, total, truncated, has_show)`.
fn search_interval_collect(wave: &Wave, s: &SearchSetup) -> (Vec<IntervalRow>, usize, bool, bool) {
    let sel: &[Sid] = &s.sel_ref;
    let clauses: &[Vec<ResolvedCond>] = &s.clauses;
    let show_sids: &[Sid] = &s.show_sids;
    let t0 = s.t0;
    let t1 = s.t1;
    let limit = s.limit;
    let verbose = s.verbose;
    let has_show = !show_sids.is_empty();
    // Interval/segment mode carries no changed() terms (setup guarantees it),
    // so clause evaluation always sees an empty transition set.
    let no_changed: BTreeSet<Sid> = BTreeSet::new();

    let mut state: BTreeMap<Sid, String> = BTreeMap::new();
    let mut results: Vec<IntervalRow> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;

    // Buffer the stream (see note in event mode).
    let mut stream: Vec<(i64, Sid, String)> = Vec::new();
    wave.for_each_event(0, Some(t1), Some(sel), |t, sid, val| {
        stream.push((t, sid, val.raw_str().into_owned()));
    });

    let mut cur_t: Option<i64> = None;
    let mut group: Vec<(Sid, String)> = Vec::new();
    let mut active = false;
    let mut seg_start: Option<i64> = None;
    let mut seg_values: Option<Vec<(String, String)>> = None;
    let mut seg_meta: Option<Json> = None;
    let mut init_checks_done = false;

    // Helper closures can't easily borrow `results`+`total`; inline the append
    // logic via a small macro-like function returning whether truncation hit.
    macro_rules! append_result {
        ($row:expr) => {{
            total += 1;
            if limit != 0 && results.len() >= limit {
                truncated = true;
                true
            } else {
                results.push($row);
                false
            }
        }};
    }

    for (t, sid, raw) in stream {
        // Level semantics: interval mode reports spans where the condition
        // *holds*. The state at t0 includes any change landing exactly at t0 (a
        // change takes effect at its own tick), so everything `<= t0` folds into
        // the baseline and the interval is anchored at t0 by the init-check
        // below. This `<= t0` deliberately differs from event mode's `< t0`
        // (edge semantics): do NOT unify them — using `< t0` here would judge
        // the t0 level from the pre-t0 state and miss a change exactly at t0.
        if t <= t0 {
            state.insert(sid, raw);
            continue;
        }
        if !init_checks_done {
            active = any_clause_holds(&state, &no_changed, clauses);
            seg_start = if active { Some(t0) } else { None };
            if active && has_show {
                seg_values = Some(show_values(wave, &state, show_sids));
                if verbose {
                    seg_meta = Some(show_meta(wave, &state, show_sids));
                }
            }
            init_checks_done = true;
        }
        if cur_t.is_none() {
            cur_t = Some(t);
        }
        if Some(t) != cur_t {
            let ct = cur_t.unwrap();
            // Apply group to state before checking.
            for (gsid, gval) in &group {
                state.insert(*gsid, gval.clone());
            }
            let cond_ok = any_clause_holds(&state, &no_changed, clauses);
            if !has_show {
                if cond_ok && !active {
                    active = true;
                    seg_start = Some(ct);
                } else if !cond_ok && active {
                    let row = IntervalRow {
                        begin_ticks: seg_start.unwrap(),
                        end_ticks: ct,
                        values: None,
                        meta: None,
                    };
                    if append_result!(row) {
                        break;
                    }
                    active = false;
                    seg_start = None;
                }
            } else if !cond_ok {
                if active {
                    let row = IntervalRow {
                        begin_ticks: seg_start.unwrap(),
                        end_ticks: ct,
                        values: seg_values.clone(),
                        meta: seg_meta.clone(),
                    };
                    if append_result!(row) {
                        break;
                    }
                    active = false;
                    seg_start = None;
                    seg_values = None;
                    seg_meta = None;
                }
            } else {
                let new_values = show_values(wave, &state, show_sids);
                if !active {
                    active = true;
                    seg_start = Some(ct);
                    seg_values = Some(new_values);
                    if verbose {
                        seg_meta = Some(show_meta(wave, &state, show_sids));
                    }
                } else if Some(&new_values) != seg_values.as_ref() {
                    let row = IntervalRow {
                        begin_ticks: seg_start.unwrap(),
                        end_ticks: ct,
                        values: seg_values.clone(),
                        meta: seg_meta.clone(),
                    };
                    if append_result!(row) {
                        break;
                    }
                    seg_start = Some(ct);
                    seg_values = Some(new_values);
                    if verbose {
                        seg_meta = Some(show_meta(wave, &state, show_sids));
                    }
                }
            }
            if truncated {
                break;
            }
            cur_t = Some(t);
            group = Vec::new();
        }
        group.push((sid, raw));
    }

    // The streaming loop only runs the initial condition check on the first
    // event with `t > t0`. If the stream contained zero such events, the check
    // never fired, so conditions that hold throughout `[t0, t1]` would emit
    // nothing. Run it now against the accumulated baseline state so a
    // file-wide-true condition still yields the full interval.
    //
    // Guarded by `t0 < t1`: a degenerate window where the user wrote
    // `--begin T --end T` describes a zero-length interval `[T, T)`; the
    // final-emit path would otherwise materialize a `[T, T)` row, which the
    // reference correctly suppresses.
    if !init_checks_done && !truncated && t0 < t1 {
        active = any_clause_holds(&state, &no_changed, clauses);
        seg_start = if active { Some(t0) } else { None };
        if active && has_show {
            seg_values = Some(show_values(wave, &state, show_sids));
            if verbose {
                seg_meta = Some(show_meta(wave, &state, show_sids));
            }
        }
        // `init_checks_done` is not read past this point; leave it as-is so
        // the warning isn't emitted under `-D warnings` in CI.
    }

    // Final pending group.
    if !group.is_empty() && !truncated {
        let ct = cur_t.unwrap();
        for (gsid, gval) in &group {
            state.insert(*gsid, gval.clone());
        }
        let cond_ok = any_clause_holds(&state, &no_changed, clauses);
        if !has_show {
            if cond_ok && !active {
                active = true;
                seg_start = Some(ct);
            } else if !cond_ok && active {
                let row = IntervalRow {
                    begin_ticks: seg_start.unwrap(),
                    end_ticks: ct,
                    values: None,
                    meta: None,
                };
                let _ = append_result!(row);
                active = false;
                seg_start = None;
            }
        } else if !cond_ok {
            if active {
                let row = IntervalRow {
                    begin_ticks: seg_start.unwrap(),
                    end_ticks: ct,
                    values: seg_values.clone(),
                    meta: seg_meta.clone(),
                };
                let _ = append_result!(row);
                active = false;
                seg_start = None;
                seg_values = None;
                seg_meta = None;
            }
        } else {
            let new_values = show_values(wave, &state, show_sids);
            if !active {
                active = true;
                seg_start = Some(ct);
                seg_values = Some(new_values);
                if verbose {
                    seg_meta = Some(show_meta(wave, &state, show_sids));
                }
            } else if Some(&new_values) != seg_values.as_ref() {
                let row = IntervalRow {
                    begin_ticks: seg_start.unwrap(),
                    end_ticks: ct,
                    values: seg_values.clone(),
                    meta: seg_meta.clone(),
                };
                let _ = append_result!(row);
                seg_start = Some(ct);
                seg_values = Some(new_values);
                if verbose {
                    seg_meta = Some(show_meta(wave, &state, show_sids));
                }
            }
        }
    }

    // Emit final interval if still active.
    if active && !truncated {
        let row = IntervalRow {
            begin_ticks: seg_start.unwrap(),
            end_ticks: t1,
            values: if has_show { seg_values.clone() } else { None },
            meta: if has_show { seg_meta.clone() } else { None },
        };
        let _ = append_result!(row);
    }

    (results, total, truncated, has_show)
}

fn search_interval_json(
    wave: &Wave,
    s: &SearchSetup,
    results: &[IntervalRow],
    total: usize,
    truncated: bool,
    has_show: bool,
) -> Json {
    let mode = if has_show { "segment" } else { "interval" };
    let rows_json: Vec<Json> = results
        .iter()
        .map(|r| {
            // `values` is `{}` rather than absent when the run named no --show
            // signals: an interval row and a segment row differ in what they
            // carry, not in which keys they have.
            let mut o = Obj::new()
                .push("begin_ticks", Json::Int(r.begin_ticks))
                .push("begin_h", Json::str(fmt_time(r.begin_ticks, s.ts)))
                .push("end_ticks", Json::Int(r.end_ticks))
                .push("end_h", Json::str(fmt_time(r.end_ticks, s.ts)))
                .push(
                    "values",
                    match &r.values {
                        Some(vals) => values_json(vals),
                        None => Json::Object(Vec::new()),
                    },
                );
            if s.verbose {
                o = o.push("meta", r.meta.clone().unwrap_or_else(|| Json::Object(Vec::new())));
            }
            o.build()
        })
        .collect();
    let show_paths: Vec<Json> = s
        .show_sids
        .iter()
        .map(|sid| Json::str(wave.signal(*sid).path.clone()))
        .collect();
    let (total_field, trunc_final) = if truncated {
        (results.len() + 1, true)
    } else {
        (total, false)
    };
    // `changed` is an event-mode notion, but it is emitted empty here rather
    // than dropped.
    let obj = Obj::new()
        .push("mode", Json::str(mode))
        .push("condition", Json::str(s.cond_label.clone()))
        .push("condition_resolved", Json::str(s.cond_text.clone()))
        .push("changed", Json::Array(Vec::new()))
        .push("show", Json::Array(show_paths))
        .push("window", window_json(s.t0, Some(s.t1), s.ts));
    let obj = push_counts(obj, results.len(), total_field, !trunc_final, trunc_final);
    let mut hints = Hints::new();
    hints.push_opt(trunc_hint(trunc_final, results.len(), total_field, false, "rows"));
    if results.is_empty() {
        hints.push(no_match_reason(s));
    }
    hints.attach(obj).push("rows", Json::Array(rows_json)).build()
}

fn search_interval_text(
    s: &SearchSetup,
    results: &[IntervalRow],
    total: usize,
    truncated: bool,
    has_show: bool,
) {
    let noun = if has_show { "segment" } else { "interval" };
    if !results.is_empty() {
        println!(
            "Found: {} {}(s)",
            count_label(if truncated { results.len() + 1 } else { total }, truncated),
            noun
        );
        for r in results {
            let bh = fmt_time(r.begin_ticks, s.ts);
            let eh = fmt_time(r.end_ticks, s.ts);
            if has_show {
                println!(
                    "  {}..{} {}",
                    ljust(&bh, 12),
                    ljust(&eh, 12),
                    values_text(r.values.as_deref().unwrap_or(&[]))
                );
            } else {
                println!("  {}..{} {}", ljust(&bh, 12), ljust(&eh, 12), s.cond_text);
            }
        }
        if truncated {
            println!("{}", trunc_line_lb(results.len(), results.len() + 1, &format!("{noun}s")));
        }
    } else {
        println!(
            "No {} in {}..{} where {}.",
            noun,
            fmt_time(s.t0, s.ts),
            fmt_time(s.t1, s.ts),
            s.cond_text
        );
    }
}
