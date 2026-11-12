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
/// renderers. Returns `(entries, shown, truncated, selective)`. `selective` says whether
/// any selection option was actually given: an empty result means something
/// different on a file with no signals in it than on a pattern that missed.
fn list_entries(
    wave: &Wave,
    args: &Args,
) -> Result<(Vec<ListEntry>, usize, bool, bool), String> {
    let limit = limit_of(args);
    let sel = selection_of(args, wave)?;
    let all = sel.is_all();

    // One row per alias path, then sort by path. A selected signal shows every
    // alias the selection did not rule out: `--scope` and `--exclude` hide the
    // rows they cover, since printing a path the user asked to drop would make
    // those options look ignored, while `--filter` hides none — it says which
    // signals are wanted, and their other paths are worth seeing.
    let mut entries: Vec<ListEntry> = Vec::new();
    for (sid, info) in wave.signals().iter().enumerate() {
        if !all && !sel.keeps_signal(info) {
            continue;
        }
        for (path, scope) in info.alias_pairs() {
            if !all && !sel.displays_alias(path, scope) {
                continue;
            }
            entries.push(ListEntry {
                path: path.to_string(),
                width: info.width,
                type_str: info.type_str,
                sid,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let total = entries.len();
    let (shown_n, trunc) = clip_len(total, limit);
    Ok((entries, shown_n, trunc, !all))
}

/// Distinct signals among the rows, and the noun the rows are counted in:
/// `paths` once an alias makes rows and signals differ.
fn row_units(entries: &[ListEntry]) -> (usize, &'static str) {
    let signals = entries.iter().map(|e| e.sid).collect::<std::collections::BTreeSet<_>>().len();
    (signals, if signals == entries.len() { "signals" } else { "paths" })
}

pub(super) fn compute_list(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let (entries, shown_n, trunc, selective) = list_entries(wave, args)?;
    let total = entries.len();
    let (_, noun) = row_units(&entries);
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
    let mut hints = Hints::new();
    hints.push_opt(trunc_hint(trunc, shown_n, total, true, noun));
    // Only blame the selection when there was one. A file whose hierarchy
    // declares nothing has no pattern to widen.
    if total == 0 {
        hints.push(if selective { SELECTION_EMPTY } else { NO_SIGNALS });
    }
    let obj = push_counts(Obj::new(), shown_n, total, true, trunc);
    let obj = hints.attach(obj).push("signals", Json::Array(sig_arr)).build();
    Ok(obj)
}

pub(super) fn text_list(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let (entries, shown_n, trunc, selective) = list_entries(wave, args)?;
    let total = entries.len();
    // Rows are alias paths; the denominator counts signals. Say both when an
    // alias makes them differ.
    let (signals, noun) = row_units(&entries);
    if total == signals {
        crate::outln!("Matched: {}/{} signals", signals, wave.signal_count());
    } else {
        crate::outln!("Matched: {}/{} signals, {} alias paths", signals, wave.signal_count(), total);
    }
    if total == 0 {
        crate::outln!(
            "{}",
            if selective {
                "no match; try a broader filter or run without --filter to browse"
            } else {
                NO_SIGNALS
            }
        );
    }
    for e in entries.iter().take(shown_n) {
        if args.verbose {
            crate::outln!(
                "  {} {}  {:<10} id={}",
                ljust(&e.path, 60),
                rjust(&e.width.to_string(), 5),
                e.type_str,
                e.sid
            );
        } else {
            crate::outln!(
                "  {} {}  {}",
                ljust(&e.path, 60),
                rjust(&e.width.to_string(), 5),
                e.type_str
            );
        }
    }
    if trunc {
        crate::outln!("{}", trunc_line(shown_n, total, noun));
    }
    Ok(())
}
