// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `summary` command: per-signal activity stats (changes, edges, first/last,
//! unique values) over a window, grouped active/static/undefined.

use crate::cli::Args;
use crate::format::{fmt_time, fmt_val};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

struct SummaryRow {
    kind: &'static str, // "active" | "static" | "undefined"
    path: String,
    value: Option<String>, // static value
    changes: usize,
    rise_count: Option<usize>,
    fall_count: Option<usize>,
    init: String,
    last: String,
    first_at: Option<i64>,
    last_at: Option<i64>,
    unique: usize,
    width: u32,
    type_str: &'static str,
}

struct SummaryCounts {
    selected: usize,
    defined: usize,
    undefined: usize,
    active: usize,
    static_: usize,
}

fn summary_rows(
    wave: &mut Wave,
    t0: i64,
    t1: Option<i64>,
    selected: &[Sid],
) -> (Vec<SummaryRow>, Vec<Sid>, SummaryCounts) {
    let init_boundary = if t0 == 0 { 0 } else { t0 - 1 };

    // Per-signal statistics, collected directly from each signal's trace. The
    // computation is independent across signals, so we process them in
    // memory-bounded batches rather than replaying a global merge — this is
    // both lighter on memory and avoids the heap entirely. Results are keyed by
    // sid and assembled (sorted) afterwards.
    struct Stats {
        sid: Sid,
        changes: usize,
        first_at: Option<i64>,
        last_at: Option<i64>,
        initial: Option<String>,
        last: Option<String>,
        unique_count: usize,
        rise_count: Option<usize>,
        fall_count: Option<usize>,
    }

    let mut stats_list: Vec<Stats> = Vec::new();
    // Signals with no value at all in the file (no trace points) are
    // "undefined" — but only if also absent from baseline. We detect defined-ness
    // per signal below.
    let mut undefined: Vec<Sid> = Vec::new();

    let batch = if should_stream(selected.len()) {
        STREAMING_BATCH
    } else {
        // Single batch (eager) for small selections.
        selected.len().max(1)
    };

    wave.for_each_signal_batched(Some(selected), batch, |sid, tr| {
        // Width/kind are resolved after the batched pass (they need a `wave`
        // borrow this closure can't hold). Here we record raw value strings and
        // provisional rise/fall counts; non-scalar rise/fall is discarded later.

        // Baseline: last change at or before init_boundary.
        let base_pos = last_at_or_before_local(&tr.times, init_boundary);

        // Window changes: indices strictly after init_boundary and <= t1.
        let start = match base_pos {
            Some(p) => p + 1,
            None => first_after_local(&tr.times, init_boundary),
        };
        let upper = match t1 {
            Some(t1) => upper_bound_local(&tr.times, t1),
            None => tr.times.len(),
        };

        let mut changes = 0usize;
        let mut first_at = None;
        let mut last_at = None;
        // Track unique value representations. Bits/Str/Event borrow their stored
        // text (no per-change allocation); Real values format into an owned key
        // (rare path), so the set holds `Cow<str>`.
        let mut uniq: std::collections::HashSet<std::borrow::Cow<str>> =
            std::collections::HashSet::new();
        let mut rise = 0usize;
        let mut fall = 0usize;

        // `prev` is a borrowed view of the previous value's bit string, used
        // only to detect clean 0->1 / 1->0 edges; non-bit values yield None.
        let mut prev: Option<&str> = base_pos.and_then(|p| bits_view(&tr.values[p]));
        if let Some(p) = base_pos {
            uniq.insert(uniq_key(&tr.values[p]));
        }

        for i in start..upper {
            let v = &tr.values[i];
            let cur = bits_view(v);
            match (prev, cur) {
                (Some("0"), Some("1")) => rise += 1,
                (Some("1"), Some("0")) => fall += 1,
                _ => {}
            }
            changes += 1;
            if first_at.is_none() {
                first_at = Some(tr.times[i]);
            }
            last_at = Some(tr.times[i]);
            prev = cur;
            uniq.insert(uniq_key(v));
        }

        let initial: Option<String> = base_pos.map(|p| raw_string(&tr.values[p]));
        let last_val: Option<String> = if upper > start {
            Some(raw_string(&tr.values[upper - 1]))
        } else {
            initial.clone()
        };
        let unique_count = uniq.len();
        let defined = base_pos.is_some() || changes > 0;
        if !defined {
            undefined.push(sid);
            return;
        }
        stats_list.push(Stats {
            sid,
            changes,
            first_at,
            last_at,
            initial,
            last: last_val,
            unique_count,
            rise_count: Some(rise),
            fall_count: Some(fall),
        });
    });

    // Resolve formatting + scalar-ness (needs the wave borrow, now free).
    let mut rows = Vec::with_capacity(stats_list.len());
    for s in &stats_list {
        let info = wave.signal(s.sid);
        let scalar = info.width == 1;
        let kind = if s.changes > 0 { "active" } else { "static" };
        let value = if kind == "static" {
            s.last.as_ref().map(|v| fmt_val(v, info.kind, info.width))
        } else {
            None
        };
        let init = match &s.initial {
            Some(v) => fmt_val(v, info.kind, info.width),
            None => "(undef)".to_string(),
        };
        let last = match &s.last {
            Some(v) => fmt_val(v, info.kind, info.width),
            None => "(undef)".to_string(),
        };
        rows.push(SummaryRow {
            kind,
            path: info.path.clone(),
            value,
            changes: s.changes,
            rise_count: if scalar { s.rise_count } else { None },
            fall_count: if scalar { s.fall_count } else { None },
            init,
            last,
            first_at: s.first_at,
            last_at: s.last_at,
            unique: s.unique_count,
            width: info.width,
            type_str: info.type_str,
        });
    }

    rows.sort_by(|a, b| a.path.cmp(&b.path));
    undefined.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));

    let active = rows.iter().filter(|r| r.kind == "active").count();
    let static_ = rows.iter().filter(|r| r.kind == "static").count();
    let counts = SummaryCounts {
        selected: selected.len(),
        defined: rows.len(),
        undefined: undefined.len(),
        active,
        static_,
    };
    (rows, undefined, counts)
}

/// Index of the last element `<= t` (binary search), or `None`.
#[inline]
fn last_at_or_before_local(times: &[i64], t: i64) -> Option<usize> {
    if times.is_empty() || times[0] > t {
        return None;
    }
    let count = times.partition_point(|&x| x <= t);
    if count == 0 { None } else { Some(count - 1) }
}

/// Index of the first element `> t` (binary search).
#[inline]
fn first_after_local(times: &[i64], t: i64) -> usize {
    times.partition_point(|&x| x <= t)
}

/// Count of elements `<= t` (i.e. exclusive upper-bound index for a window
/// ending at t, inclusive).
#[inline]
fn upper_bound_local(times: &[i64], t: i64) -> usize {
    times.partition_point(|&x| x <= t)
}

/// Render a [`RawValue`] to its canonical raw string (bits/real/string/event).
#[inline]
fn raw_string(v: &crate::backend::RawValue) -> String {
    use crate::backend::RawValue as R;
    match v {
        R::Bits(s) => s.as_str().to_string(),
        R::Real(r) => format!("{r}"),
        R::Str(s) => s.clone(),
        R::Event => String::new(),
    }
}

/// Borrow a value's bit string if it is a logic vector, else `None`. Used to
/// detect clean 0->1 / 1->0 edges without allocating.
#[inline]
fn bits_view(v: &crate::backend::RawValue) -> Option<&str> {
    match v {
        crate::backend::RawValue::Bits(s) => Some(s.as_str()),
        _ => None,
    }
}

/// A uniqueness key for a value. Bits/Str/Event borrow their stored text or a
/// constant marker (no allocation); Real values format into an owned string so
/// distinct reals count as distinct. Returns a `Cow` so the common path stays
/// allocation-free.
#[inline]
fn uniq_key(v: &crate::backend::RawValue) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match v {
        crate::backend::RawValue::Bits(s) => Cow::Borrowed(s.as_str()),
        crate::backend::RawValue::Str(s) => Cow::Borrowed(s.as_str()),
        crate::backend::RawValue::Event => Cow::Borrowed("\u{1}event"),
        crate::backend::RawValue::Real(r) => Cow::Owned(format!("{r}")),
    }
}

/// Build the verbose-only "undefined" summary rows from their sids.
fn build_undef_rows(wave: &Wave, undef_sids: &[Sid]) -> Vec<SummaryRow> {
    undef_sids
        .iter()
        .map(|sid| {
            let info = wave.signal(*sid);
            SummaryRow {
                kind: "undefined",
                path: info.path.clone(),
                value: None,
                changes: 0,
                rise_count: if info.width == 1 { Some(0) } else { None },
                fall_count: if info.width == 1 { Some(0) } else { None },
                init: "(undef)".to_string(),
                last: "(undef)".to_string(),
                first_at: None,
                last_at: None,
                unique: 0,
                width: info.width,
                type_str: info.type_str,
            }
        })
        .collect()
}

/// Computed `summary` state: the display-ordered rows (active, then static, then
/// undefined when verbose), the counts, and the resolved window.
struct SummaryData {
    ordered: Vec<SummaryRow>,
    counts: SummaryCounts,
    t0: i64,
    t1: Option<i64>,
}

fn summary_data(wave: &mut Wave, args: &Args) -> Result<SummaryData, String> {
    let ts = wave.ts_sec();
    let (t0, t1) = parse_window(args, ts)?;
    let sel = match_filter(wave, &args.filter)?;
    let selected = selected_sids(wave, &sel);

    // summary_rows loads traces in memory-bounded batches itself (the stats are
    // per-signal independent), so we do not eagerly load everything here.
    let (rows, undef_sids, counts) = summary_rows(wave, t0, t1, &selected);

    // active rows then static rows (then undefined in verbose). Partition is
    // order-preserving, so each group keeps its path-sorted order.
    let (active, statics): (Vec<SummaryRow>, Vec<SummaryRow>) =
        rows.into_iter().partition(|r| r.kind == "active");
    let mut ordered = active;
    ordered.extend(statics);
    if args.verbose {
        ordered.extend(build_undef_rows(wave, &undef_sids));
    }

    Ok(SummaryData {
        ordered,
        counts,
        t0,
        t1,
    })
}

pub(super) fn compute_summary(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let ts = wave.ts_sec();
    let d = summary_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.ordered.len();
    let (shown_n, trunc) = clip_len(total, limit);
    let begin_h = fmt_time(d.t0, ts);
    let end_h = d.t1.map(|t| fmt_time(t, ts));

    let mut row_arr = Vec::new();
    for r in d.ordered.iter().take(shown_n) {
        row_arr.push(summary_row_json(r, args.verbose, ts));
    }
    let window = Obj::new()
        .push("begin", Json::str(begin_h.clone()))
        .push("end", opt_time(end_h.as_deref()))
        .push("begin_ticks", Json::Int(d.t0))
        .push("begin_h", Json::str(begin_h.clone()))
        .push("end_ticks", Json::opt_int(d.t1))
        .push("end_h", opt_time(end_h.as_deref()))
        .build();
    let obj = Obj::new()
        .push("window", window)
        .push("selected", Json::Int(d.counts.selected as i64))
        .push("defined", Json::Int(d.counts.defined as i64))
        .push("undefined", Json::Int(d.counts.undefined as i64))
        .push("active", Json::Int(d.counts.active as i64))
        .push("static", Json::Int(d.counts.static_ as i64))
        .push("shown", Json::Int(shown_n as i64))
        .push("truncated", Json::Bool(trunc))
        .push("rows", Json::Array(row_arr))
        .build();
    Ok(obj)
}

pub(super) fn text_summary(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let d = summary_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.ordered.len();
    let (shown_n, trunc) = clip_len(total, limit);
    let begin_h = fmt_time(d.t0, ts);
    let end_h = d.t1.map(|t| fmt_time(t, ts));

    println!(
        "Window: {}..{}",
        begin_h,
        end_h.as_deref().unwrap_or("(end)")
    );
    println!(
        "Selected: {}, Defined: {}, Undefined: {}",
        d.counts.selected, d.counts.defined, d.counts.undefined
    );
    println!("Active: {}, Static: {}", d.counts.active, d.counts.static_);
    let mut current = "";
    for r in d.ordered.iter().take(shown_n) {
        if r.kind != current {
            current = r.kind;
            println!("\n{}", current.to_uppercase());
        }
        match r.kind {
            "active" => {
                let edge = match r.rise_count {
                    Some(rc) => format!(" r={} f={}", rc, r.fall_count.unwrap_or(0)),
                    None => String::new(),
                };
                if args.verbose {
                    println!(
                        "  {} w={} {} chg={}{} init={} last={} first@{} last@{} uniq={}",
                        ljust(&r.path, 45),
                        r.width,
                        r.type_str,
                        r.changes,
                        edge,
                        r.init,
                        r.last,
                        r.first_at.map(|t| fmt_time(t, ts)).unwrap_or_else(|| "-".to_string()),
                        r.last_at.map(|t| fmt_time(t, ts)).unwrap_or_else(|| "-".to_string()),
                        r.unique
                    );
                } else {
                    println!(
                        "  {} chg={}{} init={} last={}",
                        ljust(&r.path, 45),
                        r.changes,
                        edge,
                        r.init,
                        r.last
                    );
                }
            }
            "static" => {
                if args.verbose {
                    println!(
                        "  {} w={} {} value={}",
                        ljust(&r.path, 45),
                        r.width,
                        r.type_str,
                        r.value.as_deref().unwrap_or("")
                    );
                } else {
                    println!(
                        "  {} value={}",
                        ljust(&r.path, 45),
                        r.value.as_deref().unwrap_or("")
                    );
                }
            }
            _ => {
                println!(
                    "  {} w={} {}",
                    ljust(&r.path, 45),
                    r.width,
                    r.type_str
                );
            }
        }
    }
    if d.counts.defined == 0 && d.counts.undefined == 0 {
        println!("(no selected signals)");
    }
    if trunc {
        println!("{}", trunc_line(shown_n, total, "rows"));
    }
    Ok(())
}

fn summary_row_json(r: &SummaryRow, verbose: bool, ts: f64) -> Json {
    let mut o = Obj::new()
        .push("kind", Json::str(r.kind))
        .push("path", Json::str(r.path.clone()))
        .push(
            "value",
            match &r.value {
                Some(v) => Json::str(v.clone()),
                None => Json::Null,
            },
        )
        .push("changes", Json::Int(r.changes as i64))
        .push(
            "rise_count",
            match r.rise_count {
                Some(n) => Json::Int(n as i64),
                None => Json::Null,
            },
        )
        .push(
            "fall_count",
            match r.fall_count {
                Some(n) => Json::Int(n as i64),
                None => Json::Null,
            },
        )
        .push("init", Json::str(r.init.clone()))
        .push("last", Json::str(r.last.clone()));
    if let (Some(fa), Some(la)) = (r.first_at, r.last_at) {
        o = o
            .push("first_at_ticks", Json::Int(fa))
            .push("first_at", Json::str(fmt_time(fa, ts)))
            .push("first_at_h", Json::str(fmt_time(fa, ts)))
            .push("last_at_ticks", Json::Int(la))
            .push("last_at", Json::str(fmt_time(la, ts)))
            .push("last_at_h", Json::str(fmt_time(la, ts)));
    }
    if r.unique > 0 {
        o = o.push("unique", Json::Int(r.unique as i64));
    }
    if verbose {
        o = o
            .push("width", Json::Int(r.width as i64))
            .push("type", Json::str(r.type_str));
    }
    o.build()
}
