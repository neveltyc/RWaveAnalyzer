// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `dump` command: value-change events within a time window.

use crate::cli::Args;
use crate::format::fmt_time;
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// One collected `dump` event with its value already formatted. Shared by the
/// JSON and text renderers.
struct DumpRow {
    tick: i64,
    path: String,
    value: String,
    width: u32,
    type_str: &'static str,
}

/// Everything `dump` computed: the rows, the clip decision, the resolved
/// window, and what the selection actually caught.
struct DumpData {
    rows: Vec<DumpRow>,
    truncated: bool,
    t0: i64,
    t1: Option<i64>,
    selected: Vec<Sid>,
    matched: MatchReport,
}

/// Collect the in-window events (already clipped to `--limit`, value strings
/// formatted), choosing the memory-bounded collector for large/unfiltered
/// selections and the eager heap-merge for small ones — both produce the same
/// ordered rows.
fn dump_collect(wave: &mut Wave, args: &Args) -> Result<DumpData, String> {
    let ts = wave.ts_sec();
    let (t0, t1) = parse_window(args, ts)?;
    let selection = selection_of(args)?;
    let sel = match_selection(wave, &selection);
    let limit = limit_of(args);
    let selected = selected_sids(wave, &sel);
    let matched = match_report(wave, &selection, &selected);
    let sel_ref = sel.as_deref();

    let mut rows: Vec<DumpRow> = Vec::new();
    let truncated;

    // Large/unfiltered selections use the memory-bounded collector (decodes in
    // batches, retains only the earliest `limit` events); small selections load
    // eagerly and stream through the heap merge (cheaper, identical output).
    if should_stream(selected.len()) {
        let (events, _total, tr) =
            wave.collect_events_bounded(t0, t1, sel_ref, limit, STREAMING_BATCH);
        truncated = tr;
        rows.reserve(events.len());
        for e in &events {
            let info = wave.signal(e.sid);
            rows.push(DumpRow {
                tick: e.tick,
                path: info.path.clone(),
                value: fmt_value(&e.value, info.kind, info.width),
                width: info.width,
                type_str: info.type_str,
            });
        }
    } else {
        wave.ensure_loaded(&selected);
        let mut trunc = false;
        wave.for_each_event(t0, t1, sel_ref, |t, sid, val| {
            if trunc {
                return;
            }
            if limit != 0 && rows.len() >= limit {
                trunc = true;
                return;
            }
            let info = wave.signal(sid);
            rows.push(DumpRow {
                tick: t,
                path: info.path.clone(),
                value: fmt_value(val, info.kind, info.width),
                width: info.width,
                type_str: info.type_str,
            });
        });
        truncated = trunc;
    }
    Ok(DumpData { rows, truncated, t0, t1, selected, matched })
}

pub(super) fn compute_dump(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let ts = wave.ts_sec();
    let verbose = args.verbose;
    let d = dump_collect(wave, args)?;
    let (rows, truncated) = (&d.rows, d.truncated);
    let shown = rows.len();
    let mut arr: Vec<Json> = Vec::with_capacity(shown);
    let mut last_t = i64::MIN;
    let mut last_th = String::new();
    for r in rows {
        if r.tick != last_t {
            last_t = r.tick;
            last_th = fmt_time(r.tick, ts);
        }
        let mut o = Obj::new()
            .push("time_ticks", Json::Int(r.tick))
            .push("time_h", Json::str(last_th.clone()))
            .push("path", Json::str(r.path.clone()))
            .push("value", Json::str(r.value.clone()));
        if verbose {
            o = o
                .push("width", Json::Int(r.width as i64))
                .push("type", Json::str(r.type_str));
        }
        arr.push(o.build());
    }
    // Report a lower-bound total when truncated (shown + 1).
    let (total_field, trunc_final) = if truncated {
        (shown + 1, true)
    } else {
        (shown, false)
    };
    let obj = Obj::new()
        .push("window", window_json(d.t0, d.t1, ts))
        .push("matched", d.matched.json())
        .push("selected", Json::Int(d.selected.len() as i64));
    let obj = push_counts(obj, shown, total_field, !trunc_final, trunc_final);
    let mut hints = Hints::new();
    hints.push_opt(trunc_hint(trunc_final, shown, total_field, false, "events"));
    hints.push_opt(d.matched.alias_note());
    if shown == 0 {
        hints.push(empty_window_reason(wave, &d.selected, d.matched.selective, d.t0, ts));
    }
    let obj = hints.attach(obj).push("events", Json::Array(arr)).build();
    Ok(obj)
}

pub(super) fn text_dump(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let verbose = args.verbose;
    let d = dump_collect(wave, args)?;
    let (rows, truncated) = (&d.rows, d.truncated);
    let shown = rows.len();
    println!("{}", window_line(d.t0, d.t1, ts));
    if let Some(h) = d.matched.header() {
        println!("{h}");
    }
    if let Some(note) = d.matched.alias_note() {
        println!("Note: {note}");
    }
    if shown == 0 {
        println!(
            "(no events: {})",
            empty_window_reason(wave, &d.selected, d.matched.selective, d.t0, ts)
        );
        return Ok(());
    }
    let mut out = String::new();
    let mut cur = i64::MIN;
    let mut last_t = i64::MIN;
    let mut last_th = String::new();
    for r in rows {
        if r.tick != last_t {
            last_t = r.tick;
            last_th = fmt_time(r.tick, ts);
        }
        if r.tick != cur {
            cur = r.tick;
            out.push_str(&format!("T={}\n", last_th));
        }
        if verbose {
            out.push_str(&format!(
                "  {} w={} {} = {}\n",
                ljust(&r.path, 55),
                r.width,
                r.type_str,
                r.value
            ));
        } else {
            out.push_str(&format!("  {} = {}\n", ljust(&r.path, 55), r.value));
        }
    }
    print!("{out}");
    if truncated {
        println!("{}", trunc_line_lb(shown, shown + 1, "events"));
    }
    Ok(())
}
