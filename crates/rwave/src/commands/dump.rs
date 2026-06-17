// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `dump` command: value-change events within a time window.

use crate::cli::Args;
use crate::format::fmt_time;
use crate::json::{Json, Obj};
use crate::model::Wave;
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

/// Collect the in-window events (already clipped to `--limit`, value strings
/// formatted), choosing the memory-bounded collector for large/unfiltered
/// selections and the eager heap-merge for small ones — both produce the same
/// ordered rows. Returns `(rows, truncated)`.
fn dump_collect(wave: &mut Wave, args: &Args) -> Result<(Vec<DumpRow>, bool), String> {
    let ts = wave.ts_sec();
    let (t0, t1) = parse_window(args, ts)?;
    let sel = match_filter(wave, &args.filter)?;
    let limit = limit_of(args);
    let selected = selected_sids(wave, &sel);
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
    Ok((rows, truncated))
}

pub(super) fn compute_dump(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let ts = wave.ts_sec();
    let verbose = args.verbose;
    let (rows, truncated) = dump_collect(wave, args)?;
    let shown = rows.len();
    let mut arr: Vec<Json> = Vec::with_capacity(shown);
    let mut last_t = i64::MIN;
    let mut last_th = String::new();
    for r in &rows {
        if r.tick != last_t {
            last_t = r.tick;
            last_th = fmt_time(r.tick, ts);
        }
        let mut o = Obj::new()
            .push("time", Json::Int(r.tick))
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
        .push("shown", Json::Int(shown as i64))
        .push("truncated", Json::Bool(trunc_final))
        .push("events", Json::Array(arr))
        .extend(total_json_fields(total_field, trunc_final))
        .build();
    Ok(obj)
}

pub(super) fn text_dump(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let verbose = args.verbose;
    let (rows, truncated) = dump_collect(wave, args)?;
    let shown = rows.len();
    if shown == 0 {
        println!("(no changes in range)");
        return Ok(());
    }
    let mut out = String::new();
    let mut cur = i64::MIN;
    let mut last_t = i64::MIN;
    let mut last_th = String::new();
    for r in &rows {
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
