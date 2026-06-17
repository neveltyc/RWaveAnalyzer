// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Helpers shared across the per-command modules: limit/clip math, the JSON
//! count fields, signal selection, value/justify formatting, the streaming
//! threshold, and the `opt_time`/`parse_window` helpers used by more than one
//! command. Command modules pull these in with `use super::common::*;` and
//! import the domain types they need (`Json`, `Wave`, …) directly from the
//! crate.

use crate::backend::RawValue;
use crate::cli::{Args, DEFAULT_LIMIT};
use crate::filter::Filters;
use crate::format::{fmt_val, parse_time, TimeParseError, ValueKind};
use crate::json::Json;
use crate::model::{Sid, Wave};

/// Above this many selected signals, per-signal-independent commands
/// (snapshot, compare, summary) decode in memory-bounded batches rather than
/// loading every trace at once. Below it, eager loading is simpler and the
/// memory is negligible.
pub(super) const STREAMING_SIGNAL_THRESHOLD: usize = 8192;

/// Number of signals decoded per batch when streaming. Larger batches give the
/// backend more parallelism (measured sweet spot for FST decode); the cap keeps
/// peak resident trace memory bounded even for very wide vectors.
pub(super) const STREAMING_BATCH: usize = 8192;

/// Decide whether a selection of `n` signals should be processed in
/// memory-bounded batches.
#[inline]
pub(super) fn should_stream(n: usize) -> bool {
    n > STREAMING_SIGNAL_THRESHOLD
}

/// Resolve the effective row/record limit. `--verbose` disables truncation
/// unless an explicit `--limit` was supplied; `--limit 0` always means
/// unlimited. Returns `0` for "unlimited".
pub(super) fn limit_of(args: &Args) -> usize {
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
pub(super) fn clip_len(total: usize, limit: usize) -> (usize, bool) {
    if limit == 0 {
        (total, false)
    } else {
        (total.min(limit), total > limit)
    }
}

pub(super) fn trunc_line(shown: usize, total: usize, noun: &str) -> String {
    format!("... truncated: {shown}/{total} {noun} shown. (use --limit 0 to see all)")
}

pub(super) fn trunc_line_lb(shown: usize, total: usize, noun: &str) -> String {
    format!("... truncated: {shown}/{total}+ {noun} shown. (use --limit 0 to see all)")
}

pub(super) fn count_label(total: usize, truncated: bool) -> String {
    if truncated {
        format!("{total}+")
    } else {
        format!("{total}")
    }
}

/// Shared JSON count fields (`total` + `total_is_exact`).
pub(super) fn total_json_fields(total: usize, truncated: bool) -> Vec<(String, Json)> {
    vec![
        ("total".to_string(), Json::Int(total as i64)),
        ("total_is_exact".to_string(), Json::Bool(!truncated)),
    ]
}

/// Resolve a `--filter` value into an optional set of selected sids. `None`
/// means "no filter" (all signals selected).
pub(super) fn match_filter(wave: &Wave, filter: &Option<String>) -> Result<Option<Vec<Sid>>, String> {
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
pub(super) fn selected_sids(wave: &Wave, sids: &Option<Vec<Sid>>) -> Vec<Sid> {
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
pub(super) fn print_json(j: &Json) {
    println!("{}", j.to_compact_string());
}

/// Format a decoded [`RawValue`] for display using the signal's kind/width.
/// Events render as `triggered`; reals/strings print verbatim; logic vectors
/// go through `fmt_val` with the signal's `kind`.
pub(super) fn fmt_value(v: &RawValue, kind: ValueKind, width: u32) -> String {
    match v {
        RawValue::Event => "triggered".to_string(),
        RawValue::Real(_) => fmt_val(v.raw_str().as_ref(), ValueKind::Real, width),
        RawValue::Str(_) => fmt_val(v.raw_str().as_ref(), ValueKind::Str, width),
        RawValue::Bits(_) => fmt_val(v.raw_str().as_ref(), kind, width),
    }
}

/// Left-justify helper for text tables: pads with spaces on the right, never
/// truncates.
pub(super) fn ljust(s: &str, width: usize) -> String {
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

/// Right-justify helper: pads with spaces on the left, never truncates.
pub(super) fn rjust(s: &str, width: usize) -> String {
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

pub(super) fn opt_time(s: Option<&str>) -> Json {
    match s {
        Some(v) => Json::str(v),
        None => Json::Null,
    }
}

/// Parse `--begin`/`--end` into a `(t0, t1)` tick window, validating order.
pub(super) fn parse_window(args: &Args, ts: f64) -> Result<(i64, Option<i64>), String> {
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
