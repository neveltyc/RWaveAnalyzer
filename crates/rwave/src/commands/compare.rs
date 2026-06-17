// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `compare` command: signals whose value differs between two instants.

use std::collections::BTreeSet;
use crate::cli::Args;
use crate::format::{fmt_time, parse_time, TimeParseError};
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// One `compare` diff row (values already formatted).
struct Diff {
    path: String,
    at_t1: String,
    at_t2: String,
    width: u32,
    type_str: &'static str,
}

/// Computed `compare` state: the changed rows plus both resolved ticks and the
/// size of the compared union (so `unchanged` can be derived).
struct CompareData {
    diffs: Vec<Diff>,
    ta: i64,
    tb: i64,
    union_len: usize,
}

fn compare_data(wave: &mut Wave, args: &Args) -> Result<CompareData, String> {
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
    let union_len = union.len();

    let mut diffs: Vec<Diff> = Vec::new();
    for sid in &union {
        let va = sa.get(sid);
        let vb = sb.get(sid);
        // Compare by canonical raw string (a signal keeps one kind across both
        // instants), matching the analyzer's pre-format value equality: present
        // vs absent differs; two values that render identically are unchanged.
        let differs = match (va, vb) {
            (Some(a), Some(b)) => a.raw_str() != b.raw_str(),
            (None, None) => false,
            _ => true,
        };
        if differs {
            let info = wave.signal(*sid);
            let at_t1 = match va {
                Some(v) => fmt_value(v, info.kind, info.width),
                None => "(undef)".to_string(),
            };
            let at_t2 = match vb {
                Some(v) => fmt_value(v, info.kind, info.width),
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

    Ok(CompareData {
        diffs,
        ta,
        tb,
        union_len,
    })
}

pub(super) fn compute_compare(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let ts = wave.ts_sec();
    let d = compare_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.diffs.len();
    let (shown_n, trunc) = clip_len(total, limit);

    let mut arr = Vec::new();
    for df in d.diffs.iter().take(shown_n) {
        let mut o = Obj::new()
            .push("path", Json::str(df.path.clone()))
            .push("at_t1", Json::str(df.at_t1.clone()))
            .push("at_t2", Json::str(df.at_t2.clone()));
        if args.verbose {
            o = o
                .push("width", Json::Int(df.width as i64))
                .push("type", Json::str(df.type_str));
        }
        arr.push(o.build());
    }
    let t1h = fmt_time(d.ta, ts);
    let t2h = fmt_time(d.tb, ts);
    let obj = Obj::new()
        .push("t1", Json::str(t1h.clone()))
        .push("t1_ticks", Json::Int(d.ta))
        .push("t1_h", Json::str(t1h))
        .push("t2", Json::str(t2h.clone()))
        .push("t2_ticks", Json::Int(d.tb))
        .push("t2_h", Json::str(t2h))
        .push("total", Json::Int(total as i64))
        .push("shown", Json::Int(shown_n as i64))
        .push("truncated", Json::Bool(trunc))
        .push("diffs", Json::Array(arr))
        .build();
    Ok(obj)
}

pub(super) fn text_compare(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let ts = wave.ts_sec();
    let d = compare_data(wave, args)?;
    let limit = limit_of(args);
    let total = d.diffs.len();
    let (shown_n, trunc) = clip_len(total, limit);
    let unchanged = d.union_len - total;

    println!("Compare: {} vs {}", fmt_time(d.ta, ts), fmt_time(d.tb, ts));
    println!("{} changed, {} unchanged", total, unchanged);
    for df in d.diffs.iter().take(shown_n) {
        println!("  {} {} -> {}", ljust(&df.path, 48), df.at_t1, df.at_t2);
    }
    if trunc {
        println!("{}", trunc_line(shown_n, total, "diffs"));
    }
    Ok(())
}
