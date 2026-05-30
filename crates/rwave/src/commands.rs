// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Command implementations for `rwave`.
//!
//! Each `cmd_*` function reproduces the corresponding `vcd_analyzer.py`
//! command, including its JSON shape and text layout, while sourcing data from
//! [`crate::model::Wave`] (backed by `wellen`). Value comparisons use the raw
//! decoded value strings (bit strings for logic, the literal for real/string),
//! matching the reference tool, which compares pre-format values.

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::{Args, Command, DEFAULT_LIMIT};
use crate::condition::{self, Op, ParsedCondition};
use crate::filter::Filters;
use crate::format::{fmt_time, fmt_val, parse_time, TimeParseError, ValueKind};
use crate::json::{Json, Obj};
use crate::model::{OwnedValue, Sid, Wave};

/// Above this many selected signals, per-signal-independent commands
/// (snapshot, compare, summary) decode in memory-bounded batches rather than
/// loading every trace at once. Below it, eager loading is simpler and the
/// memory is negligible.
const STREAMING_SIGNAL_THRESHOLD: usize = 8192;

/// Number of signals decoded per batch when streaming. Larger batches give the
/// backend more parallelism (measured sweet spot for FST decode); the cap keeps
/// peak resident trace memory bounded even for very wide vectors.
const STREAMING_BATCH: usize = 8192;

/// Decide whether a selection of `n` signals should be processed in
/// memory-bounded batches.
#[inline]
fn should_stream(n: usize) -> bool {
    n > STREAMING_SIGNAL_THRESHOLD
}

/// Dispatch a parsed command.
pub fn run(wave: &mut Wave, args: &Args) -> Result<(), String> {
    match args.command {
        Command::Info => cmd_info(wave, args),
        Command::List => cmd_list(wave, args),
        Command::Dump => cmd_dump(wave, args),
        Command::Summary => cmd_summary(wave, args),
        Command::Snapshot => cmd_snapshot(wave, args),
        Command::Compare => cmd_compare(wave, args),
        Command::Search => cmd_search(wave, args),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Resolve the effective row/record limit. `--verbose` disables truncation
/// unless an explicit `--limit` was supplied; `--limit 0` always means
/// unlimited. Returns `0` for "unlimited".
fn limit_of(args: &Args) -> usize {
    match args.limit {
        Some(n) => n.max(0) as usize,
        None => {
            if args.verbose {
                0
            } else {
                DEFAULT_LIMIT
            }
        }
    }
}

/// Clip a slice to `limit`, returning `(shown_len, truncated)`.
fn clip_len(total: usize, limit: usize) -> (usize, bool) {
    if limit == 0 {
        (total, false)
    } else {
        (total.min(limit), total > limit)
    }
}

fn trunc_line(shown: usize, total: usize, noun: &str) -> String {
    format!("... truncated: {shown}/{total} {noun} shown.")
}

fn trunc_line_lb(shown: usize, total: usize, noun: &str) -> String {
    format!("... truncated: {shown}/{total}+ {noun} shown.")
}

fn count_label(total: usize, truncated: bool) -> String {
    if truncated {
        format!("{total}+")
    } else {
        format!("{total}")
    }
}

/// Shared JSON count fields (`total` + `total_is_exact`).
fn total_json_fields(total: usize, truncated: bool) -> Vec<(String, Json)> {
    vec![
        ("total".to_string(), Json::Int(total as i64)),
        ("total_is_exact".to_string(), Json::Bool(!truncated)),
    ]
}

/// Resolve a `--filter` value into an optional set of selected sids. `None`
/// means "no filter" (all signals selected).
fn match_filter(wave: &Wave, filter: &Option<String>) -> Result<Option<Vec<Sid>>, String> {
    let raw = match filter {
        Some(f) => f,
        None => return Ok(None),
    };
    let filters = Filters::parse_csv(raw).map_err(|e| e.0)?;
    if filters.is_empty() {
        return Ok(None);
    }
    let mut sids: Vec<Sid> = Vec::new();
    for (sid, info) in wave.signals().iter().enumerate() {
        // A signal matches if any of its alias paths matches.
        if info.aliases.iter().any(|p| filters.matches(p)) {
            sids.push(sid);
        }
    }
    Ok(Some(sids))
}

/// The set of selected sids as an explicit sorted vec (all signals if `None`).
fn selected_sids(wave: &Wave, sids: &Option<Vec<Sid>>) -> Vec<Sid> {
    match sids {
        Some(s) => {
            let mut v = s.clone();
            v.sort_unstable();
            v.dedup();
            v
        }
        None => (0..wave.signal_count()).collect(),
    }
}

/// Print a JSON value compactly followed by a newline (matches Python `print`).
fn print_json(j: &Json) {
    println!("{}", j.to_compact_string());
}

/// Format an `OwnedValue` for display using the signal's kind/width.
fn fmt_owned(v: &OwnedValue, kind: ValueKind, width: u32) -> String {
    match v {
        OwnedValue::Event => "triggered".to_string(),
        OwnedValue::Real(s) => fmt_val(s, ValueKind::Real, width),
        OwnedValue::Str(s) => fmt_val(s, ValueKind::Str, width),
        OwnedValue::Bits(s) => fmt_val(s, kind, width),
    }
}

/// Pad/truncate-free left-justify helper used for text tables. Mirrors
/// Python's `{:<width}` (pads with spaces; never truncates).
fn ljust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        for _ in 0..(width - len) {
            out.push(' ');
        }
        out
    }
}

/// Right-justify helper mirroring Python's `{:>width}`.
fn rjust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(width);
        for _ in 0..(width - len) {
            out.push(' ');
        }
        out.push_str(s);
        out
    }
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

fn cmd_info(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let (t_min, t_max) = match wave.time_range() {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    let size_bytes = std::fs::metadata(wave.path())
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    // Type counts come pre-sorted (desc count, then name) from the model.
    let type_pairs = wave.type_counts_sorted().to_vec();

    let scopes = wave.scopes();
    let comments = wave.comments();

    // Bind owned metadata once (the model returns owned strings since the
    // backend may compute them).
    let path = wave.path().to_string();
    let timescale = wave.timescale_str();
    let date = wave.date();
    let version = wave.version();
    let signal_count = wave.signal_count();
    let reference_count = wave.raw_var_count();

    let time_min_h = t_min.map(|t| fmt_time(t, ts));
    let time_max_h = t_max.map(|t| fmt_time(t, ts));
    let duration = match (t_min, t_max) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    };
    let duration_h = duration.map(|d| fmt_time(d, ts));

    if args.json {
        let mut var_types = Vec::new();
        for (k, v) in &type_pairs {
            var_types.push((k.clone(), Json::Int(*v as i64)));
        }
        let obj = Obj::new()
            .push("file", Json::str(path.clone()))
            .push("size_bytes", Json::Int(size_bytes))
            .push("timescale", Json::str(timescale.clone()))
            .push("date", Json::str(date.clone()))
            .push("version", Json::str(version.clone()))
            .push(
                "comments",
                Json::Array(comments.iter().map(|c| Json::str(c.clone())).collect()),
            )
            .push("signal_count", Json::Int(signal_count as i64))
            .push("reference_count", Json::Int(reference_count as i64))
            .push("synthesized_buses", Json::Int(0))
            .push("var_types", Json::Object(var_types))
            .push("time_min", opt_time(time_min_h.as_deref()))
            .push("time_min_ticks", Json::opt_int(t_min))
            .push("time_min_h", opt_time(time_min_h.as_deref()))
            .push("time_max", opt_time(time_max_h.as_deref()))
            .push("time_max_ticks", Json::opt_int(t_max))
            .push("time_max_h", opt_time(time_max_h.as_deref()))
            .push("duration", opt_time(duration_h.as_deref()))
            .push("duration_ticks", Json::opt_int(duration))
            .push("duration_h", opt_time(duration_h.as_deref()))
            .push(
                "scopes",
                Json::Array(scopes.iter().map(|s| Json::str(s.clone())).collect()),
            )
            .build();
        print_json(&obj);
        return Ok(());
    }

    println!("File      : {}", path);
    println!("Size      : {} bytes", thousands(size_bytes));
    if !date.is_empty() {
        println!("Date      : {}", date);
    }
    if !version.is_empty() {
        println!("Tool      : {}", version);
    }
    println!("Timescale : {}", timescale);
    if signal_count == reference_count {
        println!("Signals   : {}", signal_count);
    } else {
        println!(
            "Signals   : {} unique ({} $var refs via aliases)",
            signal_count, reference_count
        );
    }
    let types_str = type_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("Types     : {types_str}");
    println!(
        "Time      : {} ~ {} ({})",
        time_min_h.as_deref().unwrap_or("None"),
        time_max_h.as_deref().unwrap_or("None"),
        duration_h.as_deref().unwrap_or("None")
    );
    for s in &scopes {
        println!("  scope: {s}");
    }
    if !comments.is_empty() && args.verbose {
        println!("Comments  :");
        for c in &comments {
            println!("  - {c}");
        }
    }
    Ok(())
}

fn opt_time(s: Option<&str>) -> Json {
    match s {
        Some(v) => Json::str(v),
        None => Json::Null,
    }
}

/// Format an integer with thousands separators (Python `{:,}`).
fn thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn cmd_list(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let limit = limit_of(args);
    let sel = match_filter(wave, &args.filter)?;

    // Build entries: one per alias path, then sort by path.
    struct Entry {
        path: String,
        width: u32,
        type_str: &'static str,
        sid: Sid,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for (sid, info) in wave.signals().iter().enumerate() {
        if let Some(ref s) = sel {
            if !s.contains(&sid) {
                continue;
            }
        }
        for path in &info.aliases {
            entries.push(Entry {
                path: path.clone(),
                width: info.width,
                type_str: info.type_str,
                sid,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let total = entries.len();
    let (shown_n, trunc) = clip_len(total, limit);

    if args.json {
        let mut sig_arr = Vec::new();
        for e in entries.iter().take(shown_n) {
            let mut o = Obj::new()
                .push("path", Json::str(e.path.clone()))
                .push("width", Json::Int(e.width as i64))
                .push("type", Json::str(e.type_str));
            if args.verbose {
                o = o.push("id", Json::Int(e.sid as i64));
            }
            sig_arr.push(o.build());
        }
        let obj = Obj::new()
            .push("total", Json::Int(total as i64))
            .push("shown", Json::Int(shown_n as i64))
            .push("truncated", Json::Bool(trunc))
            .push("signals", Json::Array(sig_arr))
            .build();
        print_json(&obj);
        return Ok(());
    }

    println!("Matched: {}/{}", total, wave.signal_count());
    for e in entries.iter().take(shown_n) {
        println!(
            "  {} {}  {}",
            ljust(&e.path, 60),
            rjust(&e.width.to_string(), 5),
            e.type_str
        );
    }
    if trunc {
        println!("{}", trunc_line(shown_n, total, "signals"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

/// Parse `--begin`/`--end` into a `(t0, t1)` tick window, validating order.
fn parse_window(args: &Args, ts: f64) -> Result<(i64, Option<i64>), String> {
    let t0 = match &args.begin {
        Some(b) => parse_time(b, ts).map_err(|e: TimeParseError| e.0)?,
        None => 0,
    };
    let t1 = match &args.end {
        Some(e) => Some(parse_time(e, ts).map_err(|e: TimeParseError| e.0)?),
        None => None,
    };
    if let Some(t1) = t1 {
        if t1 < t0 {
            return Err("end time must be >= begin time".to_string());
        }
    }
    Ok((t0, t1))
}

fn cmd_dump(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let (t0, t1) = parse_window(args, ts)?;
    let sel = match_filter(wave, &args.filter)?;
    let limit = limit_of(args);
    let verbose = args.verbose;
    let selected = selected_sids(wave, &sel);
    let sel_ref = sel.as_deref();

    // Large/unfiltered selections use the memory-bounded collector (decodes in
    // batches, retains only the earliest `limit` events); small selections load
    // eagerly and stream through the heap merge (cheaper, identical output).
    if should_stream(selected.len()) {
        return dump_bounded(wave, sel_ref, t0, t1, limit, verbose, ts, args.json);
    }

    wave.ensure_loaded(&selected);

    if args.json {
        let mut events: Vec<Json> = Vec::new();
        let mut truncated = false;
        let mut last_t = i64::MIN;
        let mut last_th = String::new();
        wave.for_each_event(t0, t1, sel_ref, |t, sid, val| {
            if truncated {
                return;
            }
            if limit != 0 && events.len() >= limit {
                truncated = true;
                return;
            }
            let info = wave.signal(sid);
            if t != last_t {
                last_t = t;
                last_th = fmt_time(t, ts);
            }
            let raw = val.raw();
            let value = fmt_val(&raw, info.kind, info.width);
            let mut o = Obj::new()
                .push("time", Json::Int(t))
                .push("time_ticks", Json::Int(t))
                .push("time_h", Json::str(last_th.clone()))
                .push("path", Json::str(info.path.clone()))
                .push("value", Json::str(value));
            if verbose {
                o = o
                    .push("width", Json::Int(info.width as i64))
                    .push("type", Json::str(info.type_str));
            }
            events.push(o.build());
        });
        // Match the reference's lower-bound total when truncated (shown + 1).
        let (total_field, trunc_final) = if truncated {
            (events.len() + 1, true)
        } else {
            (events.len(), false)
        };
        let obj = Obj::new()
            .push("shown", Json::Int(events.len() as i64))
            .push("truncated", Json::Bool(trunc_final))
            .push("events", Json::Array(events))
            .extend(total_json_fields(total_field, trunc_final))
            .build();
        print_json(&obj);
        return Ok(());
    }

    // Text output.
    let mut out = String::new();
    let mut shown = 0usize;
    let mut truncated = false;
    let mut cur = i64::MIN;
    let mut last_t = i64::MIN;
    let mut last_th = String::new();
    wave.for_each_event(t0, t1, sel_ref, |t, sid, val| {
        if truncated {
            return;
        }
        if limit != 0 && shown >= limit {
            truncated = true;
            return;
        }
        let info = wave.signal(sid);
        if t != last_t {
            last_t = t;
            last_th = fmt_time(t, ts);
        }
        if t != cur {
            cur = t;
            out.push_str(&format!("T={}\n", last_th));
        }
        let raw = val.raw();
        let value = fmt_val(&raw, info.kind, info.width);
        if verbose {
            out.push_str(&format!(
                "  {} w={} {} = {}\n",
                ljust(&info.path, 55),
                info.width,
                info.type_str,
                value
            ));
        } else {
            out.push_str(&format!("  {} = {}\n", ljust(&info.path, 55), value));
        }
        shown += 1;
    });
    if shown == 0 {
        println!("(no changes in range)");
        return Ok(());
    }
    print!("{out}");
    if truncated {
        println!("{}", trunc_line_lb(shown, shown + 1, "events"));
    }
    Ok(())
}

/// Memory-bounded `dump` for large/unfiltered selections. Uses the model's
/// batched bounded-event collector so the whole file's histories never need to
/// be resident at once. Output is byte-identical to the streaming path.
#[allow(clippy::too_many_arguments)]
fn dump_bounded(
    wave: &mut Wave,
    sel_ref: Option<&[Sid]>,
    t0: i64,
    t1: Option<i64>,
    limit: usize,
    verbose: bool,
    ts: f64,
    json: bool,
) -> Result<(), String> {
    let (events, _total, truncated) =
        wave.collect_events_bounded(t0, t1, sel_ref, limit, STREAMING_BATCH);
    let shown = events.len();

    if json {
        let mut arr: Vec<Json> = Vec::with_capacity(shown);
        let mut last_t = i64::MIN;
        let mut last_th = String::new();
        for e in &events {
            let info = wave.signal(e.sid);
            if e.tick != last_t {
                last_t = e.tick;
                last_th = fmt_time(e.tick, ts);
            }
            let value = fmt_val(e.value.raw(), info.kind, info.width);
            let mut o = Obj::new()
                .push("time", Json::Int(e.tick))
                .push("time_ticks", Json::Int(e.tick))
                .push("time_h", Json::str(last_th.clone()))
                .push("path", Json::str(info.path.clone()))
                .push("value", Json::str(value));
            if verbose {
                o = o
                    .push("width", Json::Int(info.width as i64))
                    .push("type", Json::str(info.type_str));
            }
            arr.push(o.build());
        }
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
        print_json(&obj);
        return Ok(());
    }

    if shown == 0 {
        println!("(no changes in range)");
        return Ok(());
    }
    let mut out = String::new();
    let mut cur = i64::MIN;
    let mut last_th = String::new();
    let mut last_t = i64::MIN;
    for e in &events {
        let info = wave.signal(e.sid);
        if e.tick != last_t {
            last_t = e.tick;
            last_th = fmt_time(e.tick, ts);
        }
        if e.tick != cur {
            cur = e.tick;
            out.push_str(&format!("T={}\n", last_th));
        }
        let value = fmt_val(e.value.raw(), info.kind, info.width);
        if verbose {
            out.push_str(&format!(
                "  {} w={} {} = {}\n",
                ljust(&info.path, 55),
                info.width,
                info.type_str,
                value
            ));
        } else {
            out.push_str(&format!("  {} = {}\n", ljust(&info.path, 55), value));
        }
    }
    print!("{out}");
    if truncated {
        println!("{}", trunc_line_lb(shown, shown + 1, "events"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// snapshot
// ---------------------------------------------------------------------------

fn cmd_snapshot(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let at_raw = args.at.as_ref().ok_or("the following arguments are required: --at")?;
    let t_at = parse_time(at_raw, ts).map_err(|e: TimeParseError| e.0)?;
    let sel = match_filter(wave, &args.filter)?;
    let selected = selected_sids(wave, &sel);

    // Large/unfiltered selections decode in batches to bound memory; small
    // selections load eagerly (cheaper, identical result).
    let state = if should_stream(selected.len()) {
        wave.snapshot_streaming(t_at, Some(&selected), STREAMING_BATCH)
    } else {
        wave.ensure_loaded(&selected);
        wave.snapshot(t_at, Some(&selected))
    };

    // rows sorted by path (state keys are sids; sort by path).
    let mut known: Vec<Sid> = state.keys().copied().collect();
    known.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));

    let known_count = state.len();
    let undef: Vec<Sid> = {
        let known_set: BTreeSet<Sid> = state.keys().copied().collect();
        let mut u: Vec<Sid> = selected
            .iter()
            .copied()
            .filter(|s| !known_set.contains(s))
            .collect();
        u.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));
        u
    };

    // Build display rows (known first; undef appended only in verbose).
    struct Row {
        path: String,
        value: Option<String>,
        undefined: bool,
        width: u32,
        type_str: &'static str,
    }
    let mut rows: Vec<Row> = Vec::new();
    for sid in &known {
        let info = wave.signal(*sid);
        let v = fmt_owned(&state[sid], info.kind, info.width);
        rows.push(Row {
            path: info.path.clone(),
            value: Some(v),
            undefined: false,
            width: info.width,
            type_str: info.type_str,
        });
    }
    if args.verbose {
        for sid in &undef {
            let info = wave.signal(*sid);
            rows.push(Row {
                path: info.path.clone(),
                value: None,
                undefined: true,
                width: info.width,
                type_str: info.type_str,
            });
        }
    }

    let limit = limit_of(args);
    let total = rows.len();
    let (shown_n, trunc) = clip_len(total, limit);

    if args.json {
        let mut sig_arr = Vec::new();
        for r in rows.iter().take(shown_n) {
            let mut o = Obj::new().push("path", Json::str(r.path.clone()));
            if r.undefined {
                o = o.push("value", Json::Null).push("undefined", Json::Bool(true));
            } else {
                o = o.push("value", Json::str(r.value.clone().unwrap_or_default()));
            }
            if args.verbose {
                o = o
                    .push("width", Json::Int(r.width as i64))
                    .push("type", Json::str(r.type_str));
            }
            sig_arr.push(o.build());
        }
        let at_h = fmt_time(t_at, ts);
        let obj = Obj::new()
            .push("at", Json::str(at_h.clone()))
            .push("at_ticks", Json::Int(t_at))
            .push("at_h", Json::str(at_h))
            .push("selected", Json::Int(selected.len() as i64))
            .push("known", Json::Int(known_count as i64))
            .push("undefined", Json::Int(undef.len() as i64))
            .push("shown", Json::Int(shown_n as i64))
            .push("truncated", Json::Bool(trunc))
            .push("signals", Json::Array(sig_arr))
            .build();
        print_json(&obj);
        return Ok(());
    }

    if state.is_empty() {
        println!("No known values at {}.", fmt_time(t_at, ts));
    } else {
        println!("Known snapshot @ {}", fmt_time(t_at, ts));
    }
    if args.verbose {
        println!(
            "Selected: {}, Known: {}, Undefined: {}",
            selected.len(),
            known_count,
            undef.len()
        );
    }
    for r in rows.iter().take(shown_n) {
        if r.undefined {
            println!("  {} = (undef)", ljust(&r.path, 55));
        } else if args.verbose {
            println!(
                "  {} w={} {} = {}",
                ljust(&r.path, 55),
                r.width,
                r.type_str,
                r.value.as_deref().unwrap_or("")
            );
        } else {
            println!("  {} = {}", ljust(&r.path, 55), r.value.as_deref().unwrap_or(""));
        }
    }
    if trunc {
        println!("{}", trunc_line(shown_n, total, "signals"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

fn cmd_compare(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let at_raw = args.at.as_ref().ok_or("the following arguments are required: --at")?;
    let parts: Vec<&str> = at_raw.split(',').collect();
    if parts.len() != 2 {
        return Err("--at needs two times separated by comma, e.g. --at 17.5us,17.7us".to_string());
    }
    let ta = parse_time(parts[0].trim(), ts).map_err(|e: TimeParseError| e.0)?;
    let tb = parse_time(parts[1].trim(), ts).map_err(|e: TimeParseError| e.0)?;
    if tb < ta {
        return Err("second compare time must be >= first compare time".to_string());
    }
    let sel = match_filter(wave, &args.filter)?;
    let selected = selected_sids(wave, &sel);
    let sel_ref = sel.as_deref();

    let (sa, sb) = if should_stream(selected.len()) {
        wave.snapshot_pair_streaming(ta, tb, sel_ref, STREAMING_BATCH)
    } else {
        wave.ensure_loaded(&selected);
        wave.snapshot_pair(ta, tb, sel_ref)
    };

    // Union of sids in either snapshot, sorted by path.
    let mut union: Vec<Sid> = {
        let mut set: BTreeSet<Sid> = BTreeSet::new();
        set.extend(sa.keys().copied());
        set.extend(sb.keys().copied());
        set.into_iter().collect()
    };
    union.sort_by(|a, b| wave.signal(*a).path.cmp(&wave.signal(*b).path));

    struct Diff {
        path: String,
        at_t1: String,
        at_t2: String,
        width: u32,
        type_str: &'static str,
    }
    let mut diffs: Vec<Diff> = Vec::new();
    for sid in &union {
        let va = sa.get(sid);
        let vb = sb.get(sid);
        if va != vb {
            let info = wave.signal(*sid);
            let at_t1 = match va {
                Some(v) => fmt_owned(v, info.kind, info.width),
                None => "(undef)".to_string(),
            };
            let at_t2 = match vb {
                Some(v) => fmt_owned(v, info.kind, info.width),
                None => "(undef)".to_string(),
            };
            diffs.push(Diff {
                path: info.path.clone(),
                at_t1,
                at_t2,
                width: info.width,
                type_str: info.type_str,
            });
        }
    }

    let limit = limit_of(args);
    let total = diffs.len();
    let (shown_n, trunc) = clip_len(total, limit);
    let unchanged = union.len() - total;

    if args.json {
        let mut arr = Vec::new();
        for d in diffs.iter().take(shown_n) {
            let mut o = Obj::new()
                .push("path", Json::str(d.path.clone()))
                .push("at_t1", Json::str(d.at_t1.clone()))
                .push("at_t2", Json::str(d.at_t2.clone()));
            if args.verbose {
                o = o
                    .push("width", Json::Int(d.width as i64))
                    .push("type", Json::str(d.type_str));
            }
            arr.push(o.build());
        }
        let t1h = fmt_time(ta, ts);
        let t2h = fmt_time(tb, ts);
        let obj = Obj::new()
            .push("t1", Json::str(t1h.clone()))
            .push("t1_ticks", Json::Int(ta))
            .push("t1_h", Json::str(t1h))
            .push("t2", Json::str(t2h.clone()))
            .push("t2_ticks", Json::Int(tb))
            .push("t2_h", Json::str(t2h))
            .push("total", Json::Int(total as i64))
            .push("shown", Json::Int(shown_n as i64))
            .push("truncated", Json::Bool(trunc))
            .push("diffs", Json::Array(arr))
            .build();
        print_json(&obj);
        return Ok(());
    }

    println!("Compare: {} vs {}", fmt_time(ta, ts), fmt_time(tb, ts));
    println!("{} changed, {} unchanged", total, unchanged);
    for d in diffs.iter().take(shown_n) {
        println!("  {} {} -> {}", ljust(&d.path, 48), d.at_t1, d.at_t2);
    }
    if trunc {
        println!("{}", trunc_line(shown_n, total, "diffs"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// summary
// ---------------------------------------------------------------------------

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

fn cmd_summary(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let (t0, t1) = parse_window(args, ts)?;
    let sel = match_filter(wave, &args.filter)?;
    let selected = selected_sids(wave, &sel);

    // summary_rows loads traces in memory-bounded batches itself (the stats are
    // per-signal independent), so we do not eagerly load everything here.
    let (rows, undef_sids, counts) = summary_rows(wave, t0, t1, &selected);

    // active rows then static rows (then undefined in verbose).
    let mut ordered: Vec<&SummaryRow> = Vec::new();
    ordered.extend(rows.iter().filter(|r| r.kind == "active"));
    ordered.extend(rows.iter().filter(|r| r.kind == "static"));

    // Build undefined rows (verbose only) as owned to append.
    let undef_rows: Vec<SummaryRow> = if args.verbose {
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
    } else {
        Vec::new()
    };
    for r in &undef_rows {
        ordered.push(r);
    }

    let limit = limit_of(args);
    let total = ordered.len();
    let (shown_n, trunc) = clip_len(total, limit);
    let begin_h = fmt_time(t0, ts);
    let end_h = t1.map(|t| fmt_time(t, ts));

    if args.json {
        let mut row_arr = Vec::new();
        for r in ordered.iter().take(shown_n) {
            row_arr.push(summary_row_json(r, args.verbose, ts));
        }
        let window = Obj::new()
            .push("begin", Json::str(begin_h.clone()))
            .push("end", opt_time(end_h.as_deref()))
            .push("begin_ticks", Json::Int(t0))
            .push("begin_h", Json::str(begin_h.clone()))
            .push("end_ticks", Json::opt_int(t1))
            .push("end_h", opt_time(end_h.as_deref()))
            .build();
        let obj = Obj::new()
            .push("window", window)
            .push("selected", Json::Int(counts.selected as i64))
            .push("defined", Json::Int(counts.defined as i64))
            .push("undefined", Json::Int(counts.undefined as i64))
            .push("active", Json::Int(counts.active as i64))
            .push("static", Json::Int(counts.static_ as i64))
            .push("shown", Json::Int(shown_n as i64))
            .push("truncated", Json::Bool(trunc))
            .push("rows", Json::Array(row_arr))
            .build();
        print_json(&obj);
        return Ok(());
    }

    println!(
        "Window: {}..{}",
        begin_h,
        end_h.as_deref().unwrap_or("(end)")
    );
    println!(
        "Selected: {}, Defined: {}, Undefined: {}",
        counts.selected, counts.defined, counts.undefined
    );
    println!("Active: {}, Static: {}", counts.active, counts.static_);
    let mut current = "";
    for r in ordered.iter().take(shown_n) {
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
    if rows.is_empty() && undef_sids.is_empty() {
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

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// A resolved condition: a parsed term bound to a specific signal id.
struct ResolvedCond {
    sid: Sid,
    op: Op,
    target: condition::Target,
    width: u32,
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
        let bits = bits_of(raw);
        if !condition::condition_match(bits, raw, c.op, &c.target, c.width) {
            return false;
        }
    }
    true
}

/// Treat a raw value as a logic-bit string only if it looks like one (digits or
/// 9-state chars). Real/string raws return None (never bit-matched).
fn bits_of(raw: Option<&str>) -> Option<&str> {
    let r = raw?;
    if !r.is_empty()
        && r.chars()
            .all(|c| matches!(c.to_ascii_lowercase(), '0' | '1' | 'x' | 'z' | 'h' | 'l' | 'u' | 'w' | '-'))
    {
        Some(r)
    } else {
        None
    }
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

fn cmd_search(wave: &mut Wave, args: &Args) -> Result<(), String> {
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
    let sel_ref: Vec<Sid> = sel_vec.clone();

    let limit = limit_of(args);
    let verbose = args.verbose;
    let cond_label = condition_label(&conditions);
    let cond_text = condition_result_text(&conditions);

    if let Some(changed_sid) = changed_sid {
        return search_event_mode(
            wave,
            &sel_ref,
            &conditions,
            &show_sids,
            changed_sid,
            t0,
            t1,
            limit,
            verbose,
            &cond_label,
            &cond_text,
            args.json,
            ts,
        );
    }

    search_interval_segment_mode(
        wave,
        &sel_ref,
        &conditions,
        &show_sids,
        t0,
        t1,
        limit,
        verbose,
        &cond_label,
        &cond_text,
        args.json,
        ts,
    )
}

/// Event mode: fire when `changed_sid` truly transitions and all conditions
/// hold. Groups events by timestamp; a t=0 initialization is not a change.
#[allow(clippy::too_many_arguments)]
fn search_event_mode(
    wave: &Wave,
    sel: &[Sid],
    conditions: &[ResolvedCond],
    show_sids: &[Sid],
    changed_sid: Sid,
    t0: i64,
    t1: i64,
    limit: usize,
    verbose: bool,
    cond_label: &str,
    cond_text: &str,
    json: bool,
    ts: f64,
) -> Result<(), String> {
    struct Ev {
        time_ticks: i64,
        time_h: String,
        values: Vec<(String, String)>,
        meta: Option<Json>,
    }
    let mut state: BTreeMap<Sid, String> = BTreeMap::new();
    let mut events: Vec<Ev> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut cur_t: Option<i64> = None;
    let mut group: Vec<(Sid, String)> = Vec::new();

    // We need to process completed groups. Because for_each_event is a closure
    // callback, collect (t, sid, raw) into a buffer first for clarity. Files
    // this tool targets fit comfortably in memory; this keeps the state machine
    // identical to the reference without fighting the borrow checker.
    let mut stream: Vec<(i64, Sid, String)> = Vec::new();
    wave.for_each_event(0, Some(t1), Some(sel), |t, sid, val| {
        stream.push((t, sid, val.raw()));
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

    let _ = verbose;
    if json {
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
        let show_paths: Vec<Json> = show_sids
            .iter()
            .map(|s| Json::str(wave.signal(*s).path.clone()))
            .collect();
        let (total_field, trunc_final) = if truncated {
            (events.len() + 1, true)
        } else {
            (total, false)
        };
        let obj = Obj::new()
            .push("mode", Json::str("event"))
            .push("condition", Json::str(cond_label))
            .push("condition_resolved", Json::str(cond_text))
            .push("changed", Json::str(wave.signal(changed_sid).path.clone()))
            .push("show", Json::Array(show_paths))
            .push("begin_ticks", Json::Int(t0))
            .push("begin_h", Json::str(fmt_time(t0, ts)))
            .push("end_ticks", Json::Int(t1))
            .push("end_h", Json::str(fmt_time(t1, ts)))
            .push("shown", Json::Int(events.len() as i64))
            .push("truncated", Json::Bool(trunc_final))
            .push("events", Json::Array(evs))
            .extend(total_json_fields(total_field, trunc_final))
            .build();
        print_json(&obj);
        return Ok(());
    }

    if !events.is_empty() {
        println!(
            "Found: {} event(s)",
            count_label(if truncated { events.len() + 1 } else { total }, truncated)
        );
        for e in &events {
            println!("  T={} {}", ljust(&e.time_h, 12), values_text(&e.values));
        }
        if truncated {
            println!("{}", trunc_line_lb(events.len(), events.len() + 1, "events"));
        }
    } else {
        println!(
            "No event in {}..{} where {} changed and {}.",
            fmt_time(t0, ts),
            fmt_time(t1, ts),
            wave.signal(changed_sid).path,
            cond_text
        );
    }
    Ok(())
}

/// Interval mode (no `--show`): emit `[a, b)` intervals where conditions hold.
/// Segment mode (`--show` present): an interval further split whenever the
/// displayed show-value tuple changes while the condition remains true.
#[allow(clippy::too_many_arguments)]
fn search_interval_segment_mode(
    wave: &Wave,
    sel: &[Sid],
    conditions: &[ResolvedCond],
    show_sids: &[Sid],
    t0: i64,
    t1: i64,
    limit: usize,
    verbose: bool,
    cond_label: &str,
    cond_text: &str,
    json: bool,
    ts: f64,
) -> Result<(), String> {
    let has_show = !show_sids.is_empty();

    #[derive(Clone)]
    struct Row {
        begin_ticks: i64,
        end_ticks: i64,
        values: Option<Vec<(String, String)>>,
        meta: Option<Json>,
    }

    let mut state: BTreeMap<Sid, String> = BTreeMap::new();
    let mut results: Vec<Row> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;

    // Buffer the stream (see note in event mode).
    let mut stream: Vec<(i64, Sid, String)> = Vec::new();
    wave.for_each_event(0, Some(t1), Some(sel), |t, sid, val| {
        stream.push((t, sid, val.raw()));
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
                    let row = Row {
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
                    let row = Row {
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
                    let row = Row {
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
                let row = Row {
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
                let row = Row {
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
                let row = Row {
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
        let row = Row {
            begin_ticks: seg_start.unwrap(),
            end_ticks: t1,
            values: if has_show { seg_values.clone() } else { None },
            meta: if has_show { seg_meta.clone() } else { None },
        };
        let _ = append_result!(row);
    }

    let _ = verbose;
    if json {
        let key = if has_show { "segments" } else { "intervals" };
        let mode = if has_show { "segment" } else { "interval" };
        let rows_json: Vec<Json> = results
            .iter()
            .map(|r| {
                let mut o = Obj::new()
                    .push("begin_ticks", Json::Int(r.begin_ticks))
                    .push("begin_h", Json::str(fmt_time(r.begin_ticks, ts)))
                    .push("end_ticks", Json::Int(r.end_ticks))
                    .push("end_h", Json::str(fmt_time(r.end_ticks, ts)));
                if let Some(ref vals) = r.values {
                    o = o.push("values", values_json(vals));
                }
                if let Some(ref m) = r.meta {
                    o = o.push("meta", m.clone());
                }
                o.build()
            })
            .collect();
        let show_paths: Vec<Json> = show_sids
            .iter()
            .map(|s| Json::str(wave.signal(*s).path.clone()))
            .collect();
        let (total_field, trunc_final) = if truncated {
            (results.len() + 1, true)
        } else {
            (total, false)
        };
        let obj = Obj::new()
            .push("mode", Json::str(mode))
            .push("condition", Json::str(cond_label))
            .push("condition_resolved", Json::str(cond_text))
            .push("show", Json::Array(show_paths))
            .push("begin_ticks", Json::Int(t0))
            .push("begin_h", Json::str(fmt_time(t0, ts)))
            .push("end_ticks", Json::Int(t1))
            .push("end_h", Json::str(fmt_time(t1, ts)))
            .push("shown", Json::Int(results.len() as i64))
            .push("truncated", Json::Bool(trunc_final))
            .push(key, Json::Array(rows_json))
            .extend(total_json_fields(total_field, trunc_final))
            .build();
        print_json(&obj);
        return Ok(());
    }

    let noun = if has_show { "segment" } else { "interval" };
    if !results.is_empty() {
        println!(
            "Found: {} {}(s)",
            count_label(if truncated { results.len() + 1 } else { total }, truncated),
            noun
        );
        for r in &results {
            let bh = fmt_time(r.begin_ticks, ts);
            let eh = fmt_time(r.end_ticks, ts);
            if has_show {
                println!(
                    "  {}..{} {}",
                    ljust(&bh, 12),
                    ljust(&eh, 12),
                    values_text(r.values.as_deref().unwrap_or(&[]))
                );
            } else {
                println!("  {}..{} {}", ljust(&bh, 12), ljust(&eh, 12), cond_text);
            }
        }
        if truncated {
            println!("{}", trunc_line_lb(results.len(), results.len() + 1, &format!("{noun}s")));
        }
    } else {
        println!(
            "No {} in {}..{} where {}.",
            noun,
            fmt_time(t0, ts),
            fmt_time(t1, ts),
            cond_text
        );
    }
    Ok(())
}
