// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `info` command: file metadata — size, timescale, date/tool, signal and
//! reference counts, per-type counts, time range, scopes, and comments.

use crate::cli::Args;
use crate::format::fmt_time;
use crate::json::{Json, Obj};
use crate::model::Wave;
use super::common::*;

/// Gathered file metadata, shared by the JSON and text renderers of `info`.
struct InfoData {
    path: String,
    size_bytes: i64,
    timescale: String,
    date: String,
    version: String,
    comments: Vec<String>,
    signal_count: usize,
    reference_count: usize,
    type_pairs: Vec<(String, usize)>,
    t_min: Option<i64>,
    t_max: Option<i64>,
    duration: Option<i64>,
    time_min_h: Option<String>,
    time_max_h: Option<String>,
    duration_h: Option<String>,
    scopes: Vec<String>,
}

fn info_data(wave: &mut Wave) -> InfoData {
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

    InfoData {
        path,
        size_bytes,
        timescale,
        date,
        version,
        comments,
        signal_count,
        reference_count,
        type_pairs,
        t_min,
        t_max,
        duration,
        time_min_h,
        time_max_h,
        duration_h,
        scopes,
    }
}

pub(super) fn compute_info(wave: &mut Wave, _args: &Args) -> Result<Json, String> {
    let d = info_data(wave);
    let mut var_types = Vec::new();
    for (k, v) in &d.type_pairs {
        var_types.push((k.clone(), Json::Int(*v as i64)));
    }
    let obj = Obj::new()
        .push("file", Json::str(d.path.clone()))
        .push("size_bytes", Json::Int(d.size_bytes))
        .push("timescale", Json::str(d.timescale.clone()))
        .push("date", Json::str(d.date.clone()))
        .push("version", Json::str(d.version.clone()))
        .push(
            "comments",
            Json::Array(d.comments.iter().map(|c| Json::str(c.clone())).collect()),
        )
        .push("signal_count", Json::Int(d.signal_count as i64))
        .push("reference_count", Json::Int(d.reference_count as i64))
        .push("synthesized_buses", Json::Int(0))
        .push("var_types", Json::Object(var_types))
        .push("time_min", opt_time(d.time_min_h.as_deref()))
        .push("time_min_ticks", Json::opt_int(d.t_min))
        .push("time_min_h", opt_time(d.time_min_h.as_deref()))
        .push("time_max", opt_time(d.time_max_h.as_deref()))
        .push("time_max_ticks", Json::opt_int(d.t_max))
        .push("time_max_h", opt_time(d.time_max_h.as_deref()))
        .push("duration", opt_time(d.duration_h.as_deref()))
        .push("duration_ticks", Json::opt_int(d.duration))
        .push("duration_h", opt_time(d.duration_h.as_deref()))
        .push(
            "scopes",
            Json::Array(d.scopes.iter().map(|s| Json::str(s.clone())).collect()),
        )
        .build();
    Ok(obj)
}

pub(super) fn text_info(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let d = info_data(wave);
    println!("File      : {}", d.path);
    println!("Size      : {} bytes", thousands(d.size_bytes));
    if !d.date.is_empty() {
        println!("Date      : {}", d.date);
    }
    if !d.version.is_empty() {
        println!("Tool      : {}", d.version);
    }
    println!("Timescale : {}", d.timescale);
    if d.signal_count == d.reference_count {
        println!("Signals   : {}", d.signal_count);
    } else {
        println!(
            "Signals   : {} unique ({} $var refs via aliases)",
            d.signal_count, d.reference_count
        );
    }
    let types_str = d
        .type_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("Types     : {types_str}");
    println!(
        "Time      : {} ~ {} ({})",
        d.time_min_h.as_deref().unwrap_or("None"),
        d.time_max_h.as_deref().unwrap_or("None"),
        d.duration_h.as_deref().unwrap_or("None")
    );
    for s in &d.scopes {
        println!("  scope: {s}");
    }
    if !d.comments.is_empty() && args.verbose {
        println!("Comments  :");
        for c in &d.comments {
            println!("  - {c}");
        }
    }
    Ok(())
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
