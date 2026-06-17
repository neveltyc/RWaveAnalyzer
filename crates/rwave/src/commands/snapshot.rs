// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `snapshot` command: every selected signal's value at a single instant.

use std::collections::BTreeSet;
use crate::cli::Args;
use crate::format::{fmt_time, parse_time, TimeParseError};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// One `snapshot` display row.
struct SnapRow {
    path: String,
    value: Option<String>,
    undefined: bool,
    width: u32,
    type_str: &'static str,
}

/// Computed `snapshot` state: display rows (known first; undef appended only in
/// verbose) plus the selection/known/undef counts and the resolved tick.
struct SnapData {
    rows: Vec<SnapRow>,
    selected_len: usize,
    known_count: usize,
    undef_len: usize,
    t_at: i64,
}

fn snapshot_data(wave: &mut Wave, args: &Args) -> Result<SnapData, String> {
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
    let mut rows: Vec<SnapRow> = Vec::new();
    for sid in &known {
        let info = wave.signal(*sid);
        let v = fmt_value(&state[sid], info.kind, info.width);
        rows.push(SnapRow {
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
            rows.push(SnapRow {
                path: info.path.clone(),
                value: None,
                undefined: true,
                width: info.width,
                type_str: info.type_str,
            });
        }
    }

    Ok(SnapData {
        rows,
        selected_len: selected.len(),
        known_count,
        undef_len: undef.len(),
        t_at,
    })
}

pub(super) fn compute_snapshot(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let ts = wave.ts_sec();
    let d = snapshot_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.rows.len();
    let (shown_n, trunc) = clip_len(total, limit);

    let mut sig_arr = Vec::new();
    for r in d.rows.iter().take(shown_n) {
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
    let at_h = fmt_time(d.t_at, ts);
    let obj = Obj::new()
        .push("at", Json::str(at_h.clone()))
        .push("at_ticks", Json::Int(d.t_at))
        .push("at_h", Json::str(at_h))
        .push("selected", Json::Int(d.selected_len as i64))
        .push("known", Json::Int(d.known_count as i64))
        .push("undefined", Json::Int(d.undef_len as i64))
        .push("shown", Json::Int(shown_n as i64))
        .push("truncated", Json::Bool(trunc))
        .push("signals", Json::Array(sig_arr))
        .build();
    Ok(obj)
}

pub(super) fn text_snapshot(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let d = snapshot_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.rows.len();
    let (shown_n, trunc) = clip_len(total, limit);

    if d.known_count == 0 {
        println!("No known values at {}.", fmt_time(d.t_at, ts));
    } else {
        println!("Known snapshot @ {}", fmt_time(d.t_at, ts));
    }
    if args.verbose {
        println!(
            "Selected: {}, Known: {}, Undefined: {}",
            d.selected_len, d.known_count, d.undef_len
        );
    }
    for r in d.rows.iter().take(shown_n) {
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
