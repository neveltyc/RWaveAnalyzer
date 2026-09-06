// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `snapshot` command: every selected signal's value at a single instant.

use std::collections::BTreeSet;
use crate::cli::Args;
use crate::format::{fmt_time, parse_time, TimeParseError};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// Tail of the "nothing known here" sentence.
const NO_KNOWN_VALUE: &str =
    "every selected signal's first recorded sample is later, or it was never dumped";


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
    matched: MatchReport,
}

fn snapshot_data(wave: &mut Wave, args: &Args) -> Result<SnapData, String> {
    let ts = wave.ts_sec();
    let at_raw = args.at.as_ref().ok_or("the following arguments are required: --at")?;
    let t_at = parse_time(at_raw, ts).map_err(|e: TimeParseError| e.0)?;
    let selection = selection_of(args)?;
    let sel = match_selection(wave, &selection);
    let selected = selected_sids(wave, &sel);
    let matched = match_report(wave, &selection, &selected);

    // Large/unfiltered selections decode in batches to bound memory; small
    // selections load eagerly (cheaper, identical result). A backend that can
    // seek by time also takes the streaming path even for small selections, so
    // the point query reads just the value at `t_at` instead of full histories.
    let state = if should_stream(selected.len()) || wave.supports_windowed() {
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
        matched,
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
        // `undefined` on every row, not only the undefined ones.
        let mut o = Obj::new()
            .push("path", Json::str(r.path.clone()))
            .push(
                "value",
                match &r.value {
                    Some(v) if !r.undefined => Json::str(v.clone()),
                    _ => Json::Null,
                },
            )
            .push("undefined", Json::Bool(r.undefined));
        if args.verbose {
            o = o
                .push("width", Json::Int(r.width as i64))
                .push("type", Json::str(r.type_str));
        }
        sig_arr.push(o.build());
    }
    let obj = push_time(Obj::new(), "at", d.t_at, ts)
        .push("matched", d.matched.json())
        .push("selected", Json::Int(d.selected_len as i64))
        .push("known", Json::Int(d.known_count as i64))
        .push("undefined", Json::Int(d.undef_len as i64));
    let obj = push_counts(obj, shown_n, total, true, trunc);
    let mut hints = Hints::new();
    // `total`, not `known_count`: under --verbose the rows also carry the
    // undefined signals, so the known count is not what was clipped.
    hints.push_opt(trunc_hint(trunc, shown_n, total, true, "signals"));
    hints.push_opt(d.matched.alias_note());
    if d.selected_len == 0 {
        hints.push(if d.matched.selective { SELECTION_EMPTY } else { NO_SIGNALS });
    } else if d.known_count == 0 {
        hints.push(format!(
            "no signal has a known value at {}: {NO_KNOWN_VALUE}",
            fmt_time(d.t_at, ts)
        ));
    }
    let obj = hints.attach(obj).push("signals", Json::Array(sig_arr)).build();
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
    if let Some(h) = d.matched.header() {
        println!("{h}");
    }
    if let Some(note) = d.matched.alias_note() {
        println!("Note: {note}");
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
