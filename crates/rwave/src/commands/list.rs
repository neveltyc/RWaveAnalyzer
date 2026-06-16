// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `list` command: signal alias paths with width/type, filtered and sorted.

use crate::cli::Args;
use crate::json::{Json, Obj};
use crate::model::{Sid, Wave};
use super::common::*;

/// One `list` row: an alias path with its signal's width/type and domain id.
struct ListEntry {
    path: String,
    width: u32,
    type_str: &'static str,
    sid: Sid,
}

/// Build the sorted `list` entries and the clip decision, shared by both
/// renderers. Returns `(entries, shown, truncated)`.
fn list_entries(wave: &Wave, args: &Args) -> Result<(Vec<ListEntry>, usize, bool), String> {
    let limit = limit_of(args);
    let sel = match_filter(wave, &args.filter)?;

    // Build entries: one per alias path, then sort by path.
    let mut entries: Vec<ListEntry> = Vec::new();
    for (sid, info) in wave.signals().iter().enumerate() {
        if let Some(ref s) = sel {
            if !s.contains(&sid) {
                continue;
            }
        }
        for path in &info.aliases {
            entries.push(ListEntry {
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
    Ok((entries, shown_n, trunc))
}

pub(super) fn compute_list(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let (entries, shown_n, trunc) = list_entries(wave, args)?;
    let total = entries.len();
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
    Ok(obj)
}

pub(super) fn text_list(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let (entries, shown_n, trunc) = list_entries(wave, args)?;
    let total = entries.len();
    println!("Matched: {}/{}", total, wave.signal_count());
    if total == 0 {
        println!("no match; try a broader filter or run without --filter to browse");
    }
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
