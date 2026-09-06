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
use crate::filter::MatchMode;
use crate::format::{fmt_val, parse_time, TimeParseError, ValueKind};
use crate::json::{Json, Obj};
use crate::model::{Sid, SignalInfo, Wave};
use crate::select::Selection;

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

/// The text-mode truncation notice. Preceded by a blank line so it separates
/// from the rows above instead of reading as one more of them — a clipped
/// result that looks complete is the expensive kind of mistake.
pub(super) fn trunc_line(shown: usize, total: usize, noun: &str) -> String {
    format!(
        "\n>> TRUNCATED: showing {shown} of {total} {noun}. \
         Raise the cap with --limit N, or --limit 0 for all."
    )
}

/// As [`trunc_line`], where the total is only known to be a lower bound
/// (streaming commands stop reading once the limit is met).
pub(super) fn trunc_line_lb(shown: usize, total: usize, noun: &str) -> String {
    format!(
        "\n>> TRUNCATED: showing {shown} of {total}+ {noun}. \
         Raise the cap with --limit N, or --limit 0 for all."
    )
}

/// The truncation sentence, or `None` for a complete result. `truncated: true`
/// alone is easy to skim past; a sentence naming the flag that lifts the cap
/// is not.
pub(super) fn trunc_hint(
    trunc: bool,
    shown: usize,
    total: usize,
    exact: bool,
    noun: &str,
) -> Option<String> {
    if !trunc {
        return None;
    }
    let plus = if exact { "" } else { "+" };
    Some(format!(
        "showing {shown} of {total}{plus} {noun}; \
         re-run with --limit N (or --limit 0 for all) to see the rest"
    ))
}

pub(super) fn count_label(total: usize, truncated: bool) -> String {
    if truncated {
        format!("{total}+")
    } else {
        format!("{total}")
    }
}

/// Every sid whose signal satisfies `pred`, in ascending order. The one place
/// the signal table is scanned; selection and `search`'s name resolution both
/// come through here so they cannot drift apart.
pub(crate) fn sids_where(wave: &Wave, pred: impl Fn(&SignalInfo) -> bool) -> Vec<Sid> {
    wave.signals()
        .iter()
        .enumerate()
        .filter(|(_, info)| pred(info))
        .map(|(sid, _)| sid)
        .collect()
}

/// Compile this invocation's selection options, honouring `--exact`.
pub(super) fn selection_of(args: &Args) -> Result<Selection, String> {
    Selection::parse_mode(
        &args.scope,
        args.depth,
        &args.filter,
        &args.exclude,
        match_mode(args),
    )
}

/// The `--filter`/`--exclude` matching mode this invocation asked for.
pub(crate) fn match_mode(args: &Args) -> MatchMode {
    if args.exact {
        MatchMode::Exact
    } else {
        MatchMode::Substring
    }
}

/// Resolve a compiled selection into an optional set of selected sids. `None`
/// means "no selection given" (all signals).
///
/// Takes the [`Selection`] rather than the raw `Args` so a caller that also
/// needs the [`MatchReport`] compiles the patterns once: two compilations of
/// the same options are two chances for them to disagree about what matched.
pub(super) fn match_selection(wave: &Wave, sel: &Selection) -> Option<Vec<Sid>> {
    if sel.is_all() {
        return None;
    }
    Some(sids_where(wave, |info| sel.keeps_signal(info)))
}

/// What a selection actually caught: for each selected signal, the alias path
/// that matched and the canonical path its output rows will carry.
///
/// The two differ when a waveform declares one signal under several names, and
/// that gap is the whole reason this exists: a query for `foo_copy` answered
/// with rows labelled `foo` looks like the wrong signal came back. Empty when
/// no selection option was given — a whole-file query has nothing to report.
pub(super) struct MatchReport {
    /// `(matched alias path, canonical row path)`, sorted by the matched path.
    pub pairs: Vec<(String, String)>,
    /// True when any selection option was in force at all.
    pub selective: bool,
}

/// How many matched paths a header lists before it stops naming them. Past this
/// the list has stopped being a check on the pattern and started being the
/// output; `list` is the command for reading it in full.
const MATCH_LIST_CAP: usize = 10;

impl MatchReport {
    pub fn count(&self) -> usize {
        self.pairs.len()
    }

    /// The pairs whose matched name differs from the name the rows carry.
    pub fn aliased(&self) -> impl Iterator<Item = &(String, String)> {
        self.pairs.iter().filter(|(m, p)| m != p)
    }

    /// The one-line text header, or `None` when there is nothing to report.
    pub fn header(&self) -> Option<String> {
        if !self.selective {
            return None;
        }
        let n = self.count();
        let noun = if n == 1 { "signal" } else { "signals" };
        if n == 0 {
            return Some("Matched 0 signals".to_string());
        }
        if n > MATCH_LIST_CAP {
            return Some(format!(
                "Matched {n} {noun} (run list with the same options to see them)"
            ));
        }
        let names: Vec<String> = self
            .pairs
            .iter()
            .map(|(m, p)| if m == p { m.clone() } else { format!("{m} -> {p}") })
            .collect();
        Some(format!("Matched {n} {noun}: {}", names.join(", ")))
    }

    /// The `matched` JSON member: `null` when no selection option was given,
    /// otherwise `{count, paths}`. `count` is exact; `paths` stops at
    /// [`MATCH_LIST_CAP`], so a `paths` shorter than `count` means the rest
    /// were not listed.
    pub fn json(&self) -> Json {
        if !self.selective {
            return Json::Null;
        }
        let paths: Vec<Json> = self
            .pairs
            .iter()
            .take(MATCH_LIST_CAP)
            .map(|(m, _)| Json::str(m.clone()))
            .collect();
        Obj::new()
            .push("count", Json::Int(self.count() as i64))
            .push("paths", Json::Array(paths))
            .build()
    }

    /// The sentence warning that some rows are labelled with a name other than
    /// the one asked for, or `None` when every match came through its own
    /// canonical path.
    pub fn alias_note(&self) -> Option<String> {
        let mut it = self.aliased().peekable();
        it.peek()?;
        let shown: Vec<String> = self
            .aliased()
            .take(3)
            .map(|(m, p)| format!("{m} is {p}"))
            .collect();
        let n = self.aliased().count();
        let noun = if n == 1 { "signal" } else { "signals" };
        let more = if n > shown.len() {
            format!(", and {} more", n - shown.len())
        } else {
            String::new()
        };
        Some(format!(
            "{n} {noun} matched through an alias; rows carry the canonical path ({}{more})",
            shown.join("; ")
        ))
    }
}

/// Build the [`MatchReport`] for `sel` over the already-selected `sids`.
pub(super) fn match_report(wave: &Wave, sel: &Selection, sids: &[Sid]) -> MatchReport {
    if sel.is_all() {
        return MatchReport { pairs: Vec::new(), selective: false };
    }
    let mut pairs: Vec<(String, String)> = sids
        .iter()
        .filter_map(|sid| {
            let info = wave.signal(*sid);
            sel.matched_alias(info)
                .map(|m| (m.to_string(), info.path.clone()))
        })
        .collect();
    pairs.sort();
    MatchReport { pairs, selective: true }
}

/// Collect the sentences that explain a result, joined into one `hint` string.
///
/// `hint` already means "a sentence telling the caller what to do next", and an
/// empty result that needs explaining wants exactly that. Keeping them in one
/// field rather than adding a second keeps the JSON shape from growing another
/// conditional key.
#[derive(Default)]
pub(super) struct Hints(Vec<String>);

impl Hints {
    pub fn new() -> Hints {
        Hints(Vec::new())
    }

    pub fn push(&mut self, s: impl Into<String>) {
        self.0.push(s.into());
    }

    pub fn push_opt(&mut self, s: Option<String>) {
        if let Some(s) = s {
            self.0.push(s);
        }
    }

    fn text(&self) -> String {
        self.0.join("; ")
    }

    /// Append the joined sentences as a `hint` member — `null` when there is
    /// nothing to say, never absent.
    pub fn attach(&self, obj: Obj) -> Obj {
        if self.0.is_empty() {
            obj.push("hint", Json::Null)
        } else {
            obj.push("hint", Json::str(self.text()))
        }
    }
}

/// Said when the file's hierarchy is empty. Distinct from [`SELECTION_EMPTY`]:
/// there is no pattern to widen, so naming one would send the reader after a
/// flag they never passed.
pub(super) const NO_SIGNALS: &str =
    "the file declares no signals; its hierarchy is empty";

/// What to say when the selection options caught nothing at all. Shared so the
/// commands that report it cannot word it three different ways.
pub(super) const SELECTION_EMPTY: &str = concat!(
    "the selection matched no signals; widen --filter/--scope, ",
    "or run list to see what is there",
);

/// Said of a selection whose signals are all absent from the value-change data.
const NEVER_DUMPED_ONE: &str = concat!(
    "the selected signal carries no recorded data anywhere in the file: it is in ",
    "the hierarchy but was never written to the dump, so no window will show it",
);
const NEVER_DUMPED_MANY: &str = concat!(
    "carry no recorded data anywhere in the file: they are in the hierarchy but ",
    "were never written to the dump, so no window will show them",
);

/// Above this many selected signals the "never dumped" check is skipped.
///
/// It has to decode each selected signal to see whether it carries anything,
/// which is worth it to name the cause of a query that came back empty, and
/// not worth a full-file decode to annotate a quiet window on a million
/// signals. [`empty_window_reason`] runs the cheap tests first so the scan is
/// reached only when nothing else explains the result.
const UNDUMPED_SCAN_CAP: usize = STREAMING_BATCH;

/// How many of `sids` carry no recorded samples anywhere in the file.
///
/// A signal can be present in the hierarchy and absent from the value-change
/// data — outside the `$dumpvars` scope, or dumped by a run that did not reach
/// it. That is a different answer from "it never changed", and until now the
/// two arrived as the same empty result. `None` when the selection is too
/// large to check (see [`UNDUMPED_SCAN_CAP`]).
fn undumped_count(wave: &mut Wave, sids: &[Sid]) -> Option<usize> {
    if sids.len() > UNDUMPED_SCAN_CAP {
        return None;
    }
    let mut n = 0usize;
    wave.for_each_signal_batched(Some(sids), STREAMING_BATCH, |_, tr| {
        if tr.is_empty() {
            n += 1;
        }
    });
    Some(n)
}

/// Why a windowed command came back with nothing, when the answer is knowable.
///
/// An empty `dump` used to print the same line for every cause. There are four,
/// and they call for different fixes: the selection caught nothing (fix the
/// pattern), the signals were never dumped (fix the *simulation*, no query will
/// help), the window sits past the end of the trace (fix the time), or the
/// signals genuinely held still (the answer is "nothing happened").
pub(super) fn empty_window_reason(
    wave: &mut Wave,
    selected: &[Sid],
    selective: bool,
    t0: i64,
    ts: f64,
) -> String {
    // Cheapest first. Only the last test decodes anything, so a window that is
    // simply out of range never pays for a scan of the selection.
    if selected.is_empty() && selective {
        return SELECTION_EMPTY.to_string();
    }
    if let Some((_, mx)) = wave.time_range() {
        if t0 > mx {
            return format!(
                "the window begins at {}, after the last event at {}",
                crate::format::fmt_time(t0, ts),
                crate::format::fmt_time(mx, ts)
            );
        }
    }
    let undumped = undumped_count(wave, selected);
    if undumped == Some(selected.len()) && !selected.is_empty() {
        let n = selected.len();
        return if n == 1 {
            NEVER_DUMPED_ONE.to_string()
        } else {
            format!("the {n} selected signals {NEVER_DUMPED_MANY}")
        };
    }
    // Some of the selection was never dumped, but not all of it — the rest is
    // genuinely quiet, and both halves are worth saying.
    match undumped {
        // Positional, not `{n}`: `concat!` builds the format string at expansion
        // time, so implicit named-argument capture cannot see the bindings.
        Some(n) if n > 0 => format!(
            concat!(
                "no value changes in the window; {} of the {} selected signals ",
                "carry no recorded data at all",
            ),
            n,
            selected.len()
        ),
        _ => "no value changes in the window".to_string(),
    }
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

/// The `window` member: the parsed `--begin`/`--end` as both ticks and text.
///
/// Every command that takes a window emits this, so a run that came back empty
/// can be told apart from one whose window landed somewhere unintended. A
/// fractional value that rounded to a neighbouring tick shows up here as the
/// tick it became.
pub(super) fn window_json(t0: i64, t1: Option<i64>, ts: f64) -> Json {
    let end_h = t1.map(|t| crate::format::fmt_time(t, ts));
    Obj::new()
        .push("begin_ticks", Json::Int(t0))
        .push("begin_h", Json::str(crate::format::fmt_time(t0, ts)))
        .push("end_ticks", Json::opt_int(t1))
        .push("end_h", opt_time(end_h.as_deref()))
        .build()
}

/// A `(ticks, human)` pair under `<name>_ticks` / `<name>_h`.
///
/// The one spelling for a time in the JSON output. Each used to be written
/// three times — `time`/`time_ticks`/`time_h`, `at`/`at_ticks`/`at_h` — where
/// the bare key duplicated one of the other two, and did not even agree with
/// itself across commands: `time` was the tick count, `at` and `begin` were the
/// rendered string.
pub(super) fn push_time(obj: Obj, name: &str, ticks: i64, ts: f64) -> Obj {
    obj.push(format!("{name}_ticks"), Json::Int(ticks))
        .push(format!("{name}_h"), Json::str(crate::format::fmt_time(ticks, ts)))
}

/// [`push_time`] for a time that may be absent, writing `null` to both keys so
/// the pair is present either way.
pub(super) fn push_opt_time(obj: Obj, name: &str, ticks: Option<i64>, ts: f64) -> Obj {
    obj.push(format!("{name}_ticks"), Json::opt_int(ticks))
        .push(
            format!("{name}_h"),
            match ticks {
                Some(t) => Json::str(crate::format::fmt_time(t, ts)),
                None => Json::Null,
            },
        )
}

/// The four count fields every command carries, in one fixed order.
///
/// `shown` is what came back, `total` how many there were; `total_is_exact` is
/// false only where the command stops counting once the limit is met. Written
/// through one helper so the set and the order cannot drift per command.
pub(super) fn push_counts(obj: Obj, shown: usize, total: usize, exact: bool, trunc: bool) -> Obj {
    obj.push("shown", Json::Int(shown as i64))
        .push("truncated", Json::Bool(trunc))
        .push("total", Json::Int(total as i64))
        .push("total_is_exact", Json::Bool(exact))
}

/// The text-mode window header, matching [`window_json`].
pub(super) fn window_line(t0: i64, t1: Option<i64>, ts: f64) -> String {
    format!(
        "Window: {}..{}",
        crate::format::fmt_time(t0, ts),
        t1.map(|t| crate::format::fmt_time(t, ts)).unwrap_or_else(|| "(end)".to_string())
    )
}

pub(super) fn opt_time(s: Option<&str>) -> Json {
    match s {
        Some(v) => Json::str(v),
        None => Json::Null,
    }
}

/// Resolve one user-supplied signal name to `(canonical path, scope)`.
///
/// An exact full path (case-insensitive) wins outright, so a fully spelled name
/// is never reported as ambiguous. Otherwise the shared pattern language
/// applies, and the caller gets an error naming example matches instead of an
/// arbitrary pick. `role` is spliced into the messages so the user knows which
/// argument was at fault.
///
/// Distinct from `search`'s resolver, which resolves *within* a `--scope`
/// selection; `tree` and `trace` take a single name and no selection.
pub(super) fn resolve_signal_path(
    wave: &Wave,
    pattern: &str,
    role: &str,
) -> Result<(String, String), String> {
    let pat = pattern.trim();
    let pl = pat.to_lowercase();
    let mut exact: Vec<(String, String)> = Vec::new();
    for i in 0..wave.signal_count() {
        for (path, scope) in wave.signal(i).alias_pairs() {
            if path.to_lowercase() == pl {
                exact.push((path.to_string(), scope.to_string()));
            }
        }
    }
    exact.sort();
    exact.dedup();
    if exact.len() == 1 {
        return Ok(exact.into_iter().next().unwrap());
    }
    let filters = crate::filter::Filters::parse(&[pat]).map_err(|e| format!("{role}: {e}"))?;
    let mut hits: Vec<(String, String)> = Vec::new();
    for i in 0..wave.signal_count() {
        for (path, scope) in wave.signal(i).alias_pairs() {
            if filters.matches_path_leaf(path, crate::model::leaf_of(path, scope)) {
                hits.push((path.to_string(), scope.to_string()));
            }
        }
    }
    hits.sort();
    hits.dedup();
    match hits.len() {
        0 => Err(format!(
            "{role} matches no signals: {}",
            crate::format::pyrepr(pat)
        )),
        1 => Ok(hits.into_iter().next().unwrap()),
        n => {
            let examples: Vec<&str> = hits.iter().take(5).map(|(p, _)| p.as_str()).collect();
            Err(format!(
                "{role} {} matches {n} signals; give the full hierarchical path. Examples: {}",
                crate::format::pyrepr(pat),
                examples.join(", ")
            ))
        }
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
