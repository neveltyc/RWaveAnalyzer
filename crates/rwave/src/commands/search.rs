// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `search` command: evaluate conditions over time — event mode (a signal
//! changes while conditions hold), interval mode (spans where conditions
//! hold), and segment mode (intervals split by `--show` value changes).

use std::collections::{BTreeMap, BTreeSet};
use crate::cli::Args;
use crate::condition::{self, Op, ParsedCondition};
use crate::filter::Filters;
use crate::format::{fmt_time, fmt_val, parse_time, TimeParseError, ValueKind};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// A resolved condition: a parsed term bound to a specific signal id.
struct ResolvedCond {
    sid: Sid,
    op: Op,
    target: condition::Target,
    width: u32,
    /// Formatting class of the bound signal; decides whether the recorded value
    /// is treated as a logic-bit string during matching (see `conditions_hold`).
    kind: ValueKind,
    original: String,
    path: String,
    value_text: String,
}

/// Resolve a single signal pattern to exactly one sid. An exact full-path match
/// (case-insensitive) wins over substring matches; otherwise fall back to the
/// normal filter matcher and require a unique result.
fn resolve_one_signal(wave: &Wave, pattern: &str, role: &str) -> Result<Sid, String> {
    let pat = pattern.trim();
    let pl = pat.to_lowercase();
    let has_wild = pat.contains('*') || pat.contains('?');

    if !has_wild {
        let mut exact: Vec<Sid> = Vec::new();
        for (sid, info) in wave.signals().iter().enumerate() {
            if info.aliases.iter().any(|p| p.to_lowercase() == pl) {
                exact.push(sid);
            }
        }
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

    // Fall back to filter matching.
    let filters = Filters::parse(&[pat]).map_err(|e| e.0)?;
    let mut matched: Vec<Sid> = Vec::new();
    for (sid, info) in wave.signals().iter().enumerate() {
        if info.aliases.iter().any(|p| filters.matches(p)) {
            matched.push(sid);
        }
    }
    if matched.is_empty() {
        return Err(format!("{role} pattern {} matches no signals", crate::format::pyrepr(pattern)));
    }
    if matched.len() != 1 {
        let examples = example_paths(wave, &matched);
        let extra = if examples.is_empty() {
            String::new()
        } else {
            format!(", examples: {examples}")
        };
        return Err(format!(
            "{role} pattern {} matches {} signals; use list to choose a more specific name{extra}", crate::format::pyrepr(pattern),
            matched.len()
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
/// full-path match wins per-pattern; otherwise substring/glob matching applies.
fn resolve_show_sids(wave: &Wave, show: &Option<String>) -> Result<Vec<Sid>, String> {
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
        let mut matched_any = false;
        if !has_wild {
            let pl = pat.to_lowercase();
            let mut exact: Vec<Sid> = Vec::new();
            for (sid, info) in wave.signals().iter().enumerate() {
                if info.aliases.iter().any(|p| p.to_lowercase() == pl) {
                    exact.push(sid);
                }
            }
            if !exact.is_empty() {
                selected.extend(exact);
                continue;
            }
        }
        let filters = Filters::parse(&[pat]).map_err(|e| e.0)?;
        for (sid, info) in wave.signals().iter().enumerate() {
            if info.aliases.iter().any(|p| filters.matches(p)) {
                selected.insert(sid);
                matched_any = true;
            }
        }
        if !matched_any {
            missing.push(pat.to_string());
        }
    }
    if !missing.is_empty() {
        return Err(format!("--show matches no signals: {}", missing.join(", ")));
    }
    if selected.is_empty() {
        return Err("--show matches no signals".to_string());
    }
    let mut out: Vec<Sid> = selected.into_iter().collect();
    out.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));
    Ok(out)
}

/// Resolve the comma-separated condition string against the waveform.
fn resolve_conditions(wave: &Wave, text: &str) -> Result<Vec<ResolvedCond>, String> {
    let parsed: Vec<ParsedCondition> = condition::parse_conditions(text).map_err(|e| e.0)?;
    let mut resolved: Vec<ResolvedCond> = Vec::new();
    let mut seen: BTreeSet<(Sid, &'static str, String)> = BTreeSet::new();
    for c in parsed {
        let sid = resolve_one_signal(wave, &c.pattern, "condition signal")?;
        let op_s = c.op.as_str();
        let key = (sid, op_s, format!("{}:{:?}", c.target.raw, c.target.int.is_some()));
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        let info = wave.signal(sid);
        resolved.push(ResolvedCond {
            sid,
            op: c.op,
            target: c.target,
            width: info.width,
            kind: info.kind,
            original: c.original,
            path: info.path.clone(),
            value_text: c.value_text,
        });
    }
    Ok(resolved)
}

/// Evaluate whether all conditions hold for the given state. State maps sid to
/// the raw decoded value string; absent => undefined.
fn conditions_hold(state: &BTreeMap<Sid, String>, conds: &[ResolvedCond]) -> bool {
    for c in conds {
        let raw = state.get(&c.sid).map(|s| s.as_str());
        // Classify the recorded value as a logic-bit string by the signal's
        // declared `kind`, never by sniffing its characters. A real signal
        // renders as decimal text (e.g. 100.0 -> "100"); treating that as a bit
        // string made `dac=4` spuriously match (binary 100 == 4) and `dac=100`
        // miss. Non-logic signals (real/string/event) carry `None` here and so
        // never satisfy a numeric/bit target — only the literal-compare path.
        let bits = match c.kind {
            ValueKind::Bits => raw,
            _ => None,
        };
        if !condition::condition_match(bits, raw, c.op, &c.target, c.width) {
            return false;
        }
    }
    true
}

fn condition_label(conds: &[ResolvedCond]) -> String {
    conds.iter().map(|c| c.original.clone()).collect::<Vec<_>>().join(",")
}

fn condition_result_text(conds: &[ResolvedCond]) -> String {
    conds
        .iter()
        .map(|c| format!("{}{}{}", c.path, c.op.as_str(), c.value_text))
        .collect::<Vec<_>>()
        .join(",")
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
/// conditions, the `--show`/`--changed` selections, the loaded signal set, the
/// time window, and the display labels. Built once per invocation.
struct SearchSetup {
    conditions: Vec<ResolvedCond>,
    show_sids: Vec<Sid>,
    changed_sid: Option<Sid>,
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
        return Err("end time must be >= begin time".to_string());
    }

    let cond_text_arg = args
        .condition
        .as_ref()
        .ok_or("the following arguments are required: --condition")?;
    let conditions = resolve_conditions(wave, cond_text_arg)?;
    let mut show_sids = resolve_show_sids(wave, &args.show)?;
    let changed_sid = match &args.changed {
        Some(c) => Some(resolve_one_signal(wave, c, "changed signal")?),
        None => None,
    };
    if let Some(cs) = changed_sid {
        if show_sids.is_empty() {
            show_sids = vec![cs];
        }
    }

    // The set of signals we must load: condition signals + show + changed.
    let mut selected: BTreeSet<Sid> = conditions.iter().map(|c| c.sid).collect();
    selected.extend(show_sids.iter().copied());
    if let Some(cs) = changed_sid {
        selected.insert(cs);
    }
    let sel_vec: Vec<Sid> = selected.iter().copied().collect();
    wave.ensure_loaded(&sel_vec);

    let cond_label = condition_label(&conditions);
    let cond_text = condition_result_text(&conditions);

    Ok(SearchSetup {
        conditions,
        show_sids,
        changed_sid,
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
    if let Some(changed_sid) = s.changed_sid {
        let (events, total, truncated) = search_event_collect(wave, &s, changed_sid);
        Ok(search_event_json(wave, &s, changed_sid, &events, total, truncated))
    } else {
        let (results, total, truncated, has_show) = search_interval_collect(wave, &s);
        Ok(search_interval_json(wave, &s, &results, total, truncated, has_show))
    }
}

pub(super) fn text_search(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let s = search_setup(wave, args)?;
    if let Some(changed_sid) = s.changed_sid {
        let (events, total, truncated) = search_event_collect(wave, &s, changed_sid);
        search_event_text(wave, &s, changed_sid, &events, total, truncated);
    } else {
        let (results, total, truncated, has_show) = search_interval_collect(wave, &s);
        search_interval_text(&s, &results, total, truncated, has_show);
    }
    Ok(())
}

/// Event mode collect: fire when `changed_sid` truly transitions and all
/// conditions hold. Groups events by timestamp; a t=0 initialization is not a
/// change. Returns `(events, total, truncated)`.
fn search_event_collect(
    wave: &Wave,
    s: &SearchSetup,
    changed_sid: Sid,
) -> (Vec<Ev>, usize, bool) {
    let sel: &[Sid] = &s.sel_ref;
    let conditions: &[ResolvedCond] = &s.conditions;
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
            // Returns whether changed_sid is among the changed set after applying
            // group, AND conditions hold (evaluated post-update).
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
            changed.contains(&changed_sid) && conditions_hold(state, conditions)
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
    changed_sid: Sid,
    events: &[Ev],
    total: usize,
    truncated: bool,
) -> Json {
    let evs: Vec<Json> = events
        .iter()
        .map(|e| {
            let mut o = Obj::new()
                .push("time_ticks", Json::Int(e.time_ticks))
                .push("time_h", Json::str(e.time_h.clone()))
                .push("values", values_json(&e.values));
            if let Some(ref m) = e.meta {
                o = o.push("meta", m.clone());
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
    Obj::new()
        .push("mode", Json::str("event"))
        .push("condition", Json::str(s.cond_label.clone()))
        .push("condition_resolved", Json::str(s.cond_text.clone()))
        .push("changed", Json::str(wave.signal(changed_sid).path.clone()))
        .push("show", Json::Array(show_paths))
        .push("begin_ticks", Json::Int(s.t0))
        .push("begin_h", Json::str(fmt_time(s.t0, s.ts)))
        .push("end_ticks", Json::Int(s.t1))
        .push("end_h", Json::str(fmt_time(s.t1, s.ts)))
        .push("shown", Json::Int(events.len() as i64))
        .push("truncated", Json::Bool(trunc_final))
        .push("events", Json::Array(evs))
        .extend(total_json_fields(total_field, trunc_final))
        .build()
}

fn search_event_text(
    wave: &Wave,
    s: &SearchSetup,
    changed_sid: Sid,
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
            "No event in {}..{} where {} changed and {}.",
            fmt_time(s.t0, s.ts),
            fmt_time(s.t1, s.ts),
            wave.signal(changed_sid).path,
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
    let conditions: &[ResolvedCond] = &s.conditions;
    let show_sids: &[Sid] = &s.show_sids;
    let t0 = s.t0;
    let t1 = s.t1;
    let limit = s.limit;
    let verbose = s.verbose;
    let has_show = !show_sids.is_empty();

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
            active = conditions_hold(&state, conditions);
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
            let cond_ok = conditions_hold(&state, conditions);
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
        active = conditions_hold(&state, conditions);
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
        let cond_ok = conditions_hold(&state, conditions);
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
    let key = if has_show { "segments" } else { "intervals" };
    let mode = if has_show { "segment" } else { "interval" };
    let rows_json: Vec<Json> = results
        .iter()
        .map(|r| {
            let mut o = Obj::new()
                .push("begin_ticks", Json::Int(r.begin_ticks))
                .push("begin_h", Json::str(fmt_time(r.begin_ticks, s.ts)))
                .push("end_ticks", Json::Int(r.end_ticks))
                .push("end_h", Json::str(fmt_time(r.end_ticks, s.ts)));
            if let Some(ref vals) = r.values {
                o = o.push("values", values_json(vals));
            }
            if let Some(ref m) = r.meta {
                o = o.push("meta", m.clone());
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
    Obj::new()
        .push("mode", Json::str(mode))
        .push("condition", Json::str(s.cond_label.clone()))
        .push("condition_resolved", Json::str(s.cond_text.clone()))
        .push("show", Json::Array(show_paths))
        .push("begin_ticks", Json::Int(s.t0))
        .push("begin_h", Json::str(fmt_time(s.t0, s.ts)))
        .push("end_ticks", Json::Int(s.t1))
        .push("end_h", Json::str(fmt_time(s.t1, s.ts)))
        .push("shown", Json::Int(results.len() as i64))
        .push("truncated", Json::Bool(trunc_final))
        .push(key, Json::Array(rows_json))
        .extend(total_json_fields(total_field, trunc_final))
        .build()
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
