// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `trace`: who drives a signal, and what reads it.
//!
//! Experimental, and only for an FSDB opened through the built-in Verdi NPI
//! backend: connectivity comes from an elaborated design database, not from the
//! waveform. See [`crate::backend::design`].
//!
//! `--at T` annotates every endpoint with its value at T, from the waveform
//! already open.

use crate::backend::design::{probe_kdb, Direction, Hop, HopKind, TraceStatus};
use crate::cli::Args;
use crate::json::{Json, Obj};
use crate::model::Wave;

use super::common::*;

/// Everything both renderers need.
struct TraceData {
    signal: String,
    dir: Direction,
    kdb: String,
    status: TraceStatus,
    hops: Vec<Hop>,
    total: usize,
    shown: usize,
    truncated: bool,
    /// `--at` in ticks and its human spelling, when value annotation was asked for.
    at: Option<(i64, String)>,
    /// Endpoint path -> formatted value at `--at`. Absent means the design named
    /// a signal the waveform does not carry, which is normal and not an error.
    values: std::collections::HashMap<String, String>,
    /// Endpoint names the design reported but the waveform does not contain.
    unresolved_in_wave: usize,
}

/// Shown when the open file cannot answer connectivity questions.
fn unsupported_message(wave: &Wave) -> String {
    // Plugin-backed formats report `Unknown`, so name the file's extension
    // instead.
    let tag = wave.file_format().tag();
    let fmt = if tag == "unknown" {
        std::path::Path::new(wave.path())
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| tag.to_string())
    } else {
        tag.to_string()
    };
    let mut s = format!(
        "trace requires an FSDB opened through the built-in Verdi NPI backend; \
         '{fmt}' has no design data."
    );
    // Only actionable for an .fsdb; unsetting it cannot give a VCD design data.
    if fmt == "fsdb" && std::env::var_os("RWAVE_PLUGIN_FSDB").is_some() {
        s.push_str("\nUnset RWAVE_PLUGIN_FSDB, which replaces that backend.");
    }
    s
}

fn build(wave: &mut Wave, args: &Args) -> Result<TraceData, String> {
    let target = args
        .target
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "the following arguments are required: <signal> (rwave trace <file> <signal>)"
                .to_string()
        })?;
    // Drivers by default; `--load` flips it.
    let dir = if args.load { Direction::Load } else { Direction::Driver };
    // NPI filters control dependencies at the source, so names are never
    // pattern-matched to find clocks and resets.
    let control = args.control;

    // Parse --at before anything expensive: loading a design checks out a
    // licence, and a mistyped time should not cost that.
    let at_ticks = match args.at.as_deref().filter(|s| !s.trim().is_empty()) {
        None => None,
        Some(spec) => {
            let ts = wave.ts_sec();
            Some((crate::format::parse_time(spec, ts).map_err(|e| e.0)?, ts))
        }
    };

    // Capability first: an ambiguous name is irrelevant if this file can never
    // answer at all.
    if wave.design_query().is_none() {
        return Err(unsupported_message(wave));
    }

    // Resolve against the waveform, so NPI gets a full hierarchical path and
    // the user gets rwave's own error messages.
    let (path, _scope) = resolve_signal_path(wave, target, "signal")?;

    // The waveform names the design it came from; `--kdb` overrides it.
    let recorded = wave.design_query().and_then(|dq| dq.recorded_design_dir());
    let kdb = probe_kdb(args.kdb.as_deref(), recorded.as_deref())
        .map_err(|m| m.into_error())?;

    let dq = wave
        .design_query()
        .expect("capability re-checked after the probe");
    dq.ensure_design(&kdb, args.top.as_deref())?;
    let outcome = dq.trace(&path, dir, control)?;

    let total = outcome.hops.len();
    let limit = limit_of(args);
    let (shown, truncated) = clip_len(total, limit);
    let mut hops = outcome.hops;
    hops.truncate(shown);

    // Value annotation runs after the design borrow ends, over the endpoints we
    // are actually going to print.
    let mut values = std::collections::HashMap::new();
    let mut unresolved_in_wave = 0usize;
    let at = match at_ticks {
        None => None,
        Some((t, ts)) => {
            let mut wanted: Vec<String> = Vec::new();
            for h in &hops {
                for s in &h.signals {
                    if !wanted.contains(s) {
                        wanted.push(s.clone());
                    }
                }
            }
            // One pass over the signal table instead of one per endpoint.
            let index = endpoint_index(wave, &wanted);
            let mut sids = Vec::new();
            let mut pairs = Vec::new();
            for name in &wanted {
                match index.get(name) {
                    Some(&sid) => {
                        sids.push(sid);
                        pairs.push((name.clone(), sid));
                    }
                    None => unresolved_in_wave += 1,
                }
            }
            let state = if wave.supports_windowed() {
                wave.snapshot_streaming(t, Some(&sids), STREAMING_BATCH)
            } else {
                wave.ensure_loaded(&sids);
                wave.snapshot(t, Some(&sids))
            };
            for (name, sid) in pairs {
                if let Some(v) = state.get(&sid) {
                    let info = wave.signal(sid);
                    values.insert(name, fmt_value(v, info.kind, info.width));
                }
            }
            Some((t, crate::format::fmt_time(t, ts)))
        }
    };

    Ok(TraceData {
        signal: path,
        dir,
        kdb: kdb.display().to_string(),
        status: outcome.status,
        hops,
        total,
        shown,
        truncated,
        at,
        values,
        unresolved_in_wave,
    })
}

pub(super) fn compute_trace(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    let d = build(wave, args)?;
    let mut o = Obj::new()
        .push("signal", Json::str(d.signal.clone()))
        .push("dir", Json::str(d.dir.tag()))
        // Reserved for a future time-aware ("active") trace, which reports only
        // the driver in effect at T rather than every structural driver.
        .push("mode", Json::str("static"))
        .push("kdb", Json::str(d.kdb.clone()))
        .push("status", Json::str(d.status.tag()));
    if let Some((ticks, human)) = &d.at {
        o = o
            .push("at_ticks", Json::Int(*ticks))
            .push("at_h", Json::str(human.clone()))
            .push("unresolved_in_wave", Json::Int(d.unresolved_in_wave as i64));
    }
    o = o
        .push("total", Json::Int(d.total as i64))
        .push("shown", Json::Int(d.shown as i64))
        .push("truncated", Json::Bool(d.truncated));
    let noun = if d.dir == Direction::Driver { "drivers" } else { "loads" };
    o = push_trunc_hint(o, d.truncated, d.shown, d.total, true, noun);

    let rows: Vec<Json> = d
        .hops
        .iter()
        .map(|h| {
            let sigs: Vec<Json> = h
                .signals
                .iter()
                .map(|s| {
                    let mut so = Obj::new().push("path", Json::str(s.clone()));
                    if d.at.is_some() {
                        so = so.push(
                            "value",
                            match d.values.get(s) {
                                Some(v) => Json::str(v.clone()),
                                None => Json::Null,
                            },
                        );
                    }
                    so.build()
                })
                .collect();
            Obj::new()
                .push("group", Json::Int(h.group as i64))
                .push("kind", Json::str(h.kind.tag()))
                .push("npi_type", Json::str(h.npi_type.clone()))
                .push("statement", Json::str(h.statement.clone()))
                .push("scope", Json::str(h.scope.clone()))
                .push("file", Json::opt_str(h.file.as_deref()))
                .push("line", Json::opt_int(h.line.map(|l| l as i64)))
                .push("boundary", Json::Bool(h.boundary))
                .push("signals", Json::Array(sigs))
                .build()
        })
        .collect();
    Ok(o.push(noun, Json::Array(rows)).build())
}

/// Map each wanted endpoint name to a sid. Built once per `--at` query, since
/// probing the signal table per name is quadratic on a design with a million
/// signals.
fn endpoint_index(
    wave: &Wave,
    wanted: &[String],
) -> std::collections::HashMap<String, crate::model::Sid> {
    use std::collections::{HashMap, HashSet};
    let exact_want: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let lower_want: HashSet<String> = wanted.iter().map(|w| w.to_lowercase()).collect();

    let mut exact: HashMap<String, crate::model::Sid> = HashMap::with_capacity(wanted.len());
    // Case-insensitive candidates, kept only while unambiguous. SystemVerilog
    // identifiers are case-sensitive, so `req` and `REQ` can both exist; folding
    // case and taking the first hit would show one signal's value under the
    // other's name. These names come from NPI verbatim, so an exact match is
    // the normal outcome and folding is only a fallback for a backend that
    // spells hierarchies differently.
    let mut folded: HashMap<String, Option<crate::model::Sid>> = HashMap::new();

    for sid in 0..wave.signal_count() {
        for (path, _) in wave.signal(sid).alias_pairs() {
            if exact_want.contains(path) {
                exact.entry(path.to_string()).or_insert(sid);
            }
            let lower = path.to_lowercase();
            if lower_want.contains(&lower) {
                folded
                    .entry(lower)
                    .and_modify(|slot| {
                        if *slot != Some(sid) {
                            *slot = None;
                        }
                    })
                    .or_insert(Some(sid));
            }
        }
    }

    let mut out = exact;
    for name in wanted {
        if out.contains_key(name) {
            continue;
        }
        if let Some(Some(sid)) = folded.get(&name.to_lowercase()) {
            out.insert(name.clone(), *sid);
        }
    }
    out
}

/// `file:line`, basename only — NPI reports the *build host's* absolute path,
/// which is long and almost never the reader's own checkout.
fn loc_of(h: &Hop) -> String {
    match (&h.file, h.line) {
        (Some(f), Some(l)) => {
            let base = f.rsplit(['/', '\\']).next().unwrap_or(f);
            format!("{base}:{l}")
        }
        _ => String::new(),
    }
}

pub(super) fn text_trace(wave: &mut Wave, args: &Args) -> Result<(), String> {
    let d = build(wave, args)?;
    let noun = match (d.dir, d.total) {
        (Direction::Driver, 1) => "driver".to_string(),
        (Direction::Driver, n) => format!("{n} drivers"),
        (Direction::Load, 1) => "load".to_string(),
        (Direction::Load, n) => format!("{n} loads"),
    };
    let head = if d.total == 1 { format!("1 {noun}") } else { noun };
    println!("{} — {head}", d.signal);
    if d.hops.is_empty() {
        // The header already said "0 drivers"; a status line would repeat it.
        if args.verbose {
            println!("\nkdb: {}", d.kdb);
        }
        return Ok(());
    }
    println!();

    // Columns, so a long load list scans vertically instead of having to be
    // read line by line. Widths come from the data, as elsewhere in rwave.
    let kind_w = d.hops.iter().map(|h| h.kind.tag().len()).max().unwrap_or(0);
    let loc_w = d.hops.iter().map(|h| loc_of(h).chars().count()).max().unwrap_or(0);
    // `from:` for a driver (data arrives from these), `to:` for a load (it goes
    // to these). Naming the relation is the point — an unlabelled indented line
    // does not say how it relates to the statement above it.
    let rel = if d.dir == Direction::Driver { "from" } else { "to" };

    // A signal has ~one driver but can have many loads, so the two directions
    // want different shapes. A driver is an answer and gets its operands spelled
    // out; a load list is an inventory and stays one line per entry, because 50
    // loads at two lines each is not something anyone reads. `--at` and
    // `--verbose` opt back into the detail (with `--at` the values *are* the
    // reason for asking), and the full structure is always in `--json`.
    let detail = d.dir == Direction::Driver || d.at.is_some() || args.verbose;

    for h in &d.hops {
        // A port hop's statement is just the port name, which says nothing on
        // its own; what matters is the net on the other side. Fold it inline so
        // the one-line form stays informative.
        let content = match (detail, h.kind, h.signals.first()) {
            (false, HopKind::Port, Some(s)) => format!("-> {s}"),
            _ => h.statement.clone(),
        };
        // `boundary` stays in --json only: for a `port` hop it is nearly always
        // true, so on screen it is a column that never distinguishes anything.
        println!(
            "  {}  {}  {}",
            ljust(h.kind.tag(), kind_w),
            ljust(&loc_of(h), loc_w),
            content
        );
        if !detail {
            continue;
        }
        for s in &h.signals {
            let val = match d.values.get(s) {
                Some(v) => format!(" = {v}"),
                None => String::new(),
            };
            // Line up under the statement column: the two separators between
            // kind/location/statement have to be counted too.
            println!("  {}{rel}: {s}{val}", ljust("", kind_w + loc_w + 4));
        }
    }

    // Everything below is printed only when it carries information the caller
    // does not already have. A `status: resolved` line after a list of drivers
    // says nothing, and echoing back the `--at` they just typed says less.
    if d.status != TraceStatus::Resolved {
        println!();

        // Only the two statuses that can accompany a non-empty list; an empty
        // one returned above, and `Resolved` is what the list already shows.
        if d.status == TraceStatus::BoundaryOnly {
            println!("driver is outside the traced hierarchy");
        }
    }
    if d.unresolved_in_wave > 0 {
        println!("\n{} endpoint(s) not dumped in this waveform", d.unresolved_in_wave);
    }
    if args.verbose {
        println!("\nkdb: {}", d.kdb);
    }
    if d.truncated {
        let n = if d.dir == Direction::Driver { "drivers" } else { "loads" };
        println!("{}", trunc_line(d.shown, d.total, n));
    }
    Ok(())
}
