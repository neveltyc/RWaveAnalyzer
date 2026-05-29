// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Domain model: the format-neutral view of a waveform that the command layer
//! operates on.
//!
//! This layer sits between the command set and a [`WaveformBackend`]. It owns
//! the backend behind a trait object, builds a stable, sorted, alias-merged
//! signal table from the backend's declarations, and exposes the domain
//! operations the commands need: signal lookup/selection, value-change replay
//! in time order, and point/pair snapshots. It contains **no** parser- or
//! format-specific code — everything file-specific lives behind the backend.
//!
//! ## Ticks and time
//!
//! A "tick" is the raw integer timestamp from the file's time axis. The
//! timescale (seconds-per-tick) is carried separately, exactly as the analyzer
//! surface expects, so that bare-tick arithmetic stays exact and unit
//! conversion is applied only at formatting time.
//!
//! ## Replay performance
//!
//! Selected signals are decoded once by the backend into owned
//! [`SignalTrace`]s and cached here. Time-ordered replay across multiple
//! signals is a k-way merge implemented with a binary min-heap, so emitting
//! `n` changes across `k` signals costs `O(n log k)` rather than the
//! `O(n · k)` of a per-step linear scan. Each heap entry carries a precomputed
//! current tick and a declaration-order key, so the hot loop performs no
//! re-lookup and ties resolve to writer order without extra work.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::backend::{
    BackendSid, FileFormat, RawValue, SignalTrace, WaveformBackend,
};
use crate::format::ValueKind;

/// Dense, domain-level signal id, assigned in sorted-by-path order so output
/// ordering is deterministic and independent of file iteration order.
pub type Sid = usize;

/// Per-signal metadata in the domain model. Backend-neutral: no parser handles
/// leak through except [`backend_sid`], which is opaque.
#[derive(Debug, Clone)]
pub struct SignalInfo {
    /// Canonical (first, lexicographically smallest) full path.
    pub path: String,
    /// All alias paths mapping to the same underlying signal, sorted.
    pub aliases: Vec<String>,
    /// Bit width (1 for scalars/real/string; declared width for vectors).
    pub width: u32,
    /// Canonical type string (`wire`, `reg`, `real`, `event`, ...).
    pub type_str: &'static str,
    /// Value formatting class.
    pub kind: ValueKind,
    /// Parent scope paths across this signal's aliases.
    pub scopes: Vec<String>,
    /// Smallest declaration index among aliases; ties timestamp-coincident
    /// events back to writer order during replay.
    pub decl_order: usize,
    /// Opaque backend handle used to request this signal's trace.
    backend_sid: BackendSid,
}

/// A decoded value as seen by the command layer. Owns its payload, so replay
/// never holds a backend borrow. This mirrors [`RawValue`] but lives in the
/// domain layer; the two are converted at the model boundary.
#[derive(Debug, Clone)]
pub enum ValueRef {
    Bits(String),
    Real(f64),
    Str(String),
    Event,
}

impl ValueRef {
    /// The raw string used by [`crate::format::fmt_val`] and for comparisons.
    pub fn raw(&self) -> String {
        match self {
            ValueRef::Bits(s) => s.clone(),
            ValueRef::Real(r) => fmt_real(*r),
            ValueRef::Str(s) => s.clone(),
            ValueRef::Event => String::new(),
        }
    }

    fn from_raw(v: &RawValue) -> ValueRef {
        match v {
            RawValue::Bits(s) => ValueRef::Bits(s.clone()),
            RawValue::Real(r) => ValueRef::Real(*r),
            RawValue::Str(s) => ValueRef::Str(s.clone()),
            RawValue::Event => ValueRef::Event,
        }
    }
}

/// An owned value used in snapshots and cross-time comparisons. Two values are
/// equal iff their canonical raw strings are equal, matching the analyzer's
/// pre-format comparison semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedValue {
    Bits(String),
    Real(String),
    Str(String),
    Event,
}

impl OwnedValue {
    pub fn raw(&self) -> &str {
        match self {
            OwnedValue::Bits(s) => s,
            OwnedValue::Real(s) => s,
            OwnedValue::Str(s) => s,
            OwnedValue::Event => "",
        }
    }
}

impl From<&ValueRef> for OwnedValue {
    fn from(v: &ValueRef) -> Self {
        match v {
            ValueRef::Bits(s) => OwnedValue::Bits(s.clone()),
            ValueRef::Real(r) => OwnedValue::Real(fmt_real(*r)),
            ValueRef::Str(s) => OwnedValue::Str(s.clone()),
            ValueRef::Event => OwnedValue::Event,
        }
    }
}

/// Render a real value compactly and round-trippably (Rust's default float
/// formatting), matching how reals are carried as their literal payload.
fn fmt_real(r: f64) -> String {
    format!("{r}")
}

/// The loaded waveform: a backend plus the derived signal table and a cache of
/// decoded traces keyed by domain [`Sid`].
pub struct Wave {
    backend: Box<dyn WaveformBackend>,
    signals: Vec<SignalInfo>,
    raw_var_count: usize,
    type_counts: Vec<(String, usize)>,
    /// Cache of decoded traces, indexed by `Sid`. `None` until first load.
    traces: Vec<Option<SignalTrace>>,
}

/// Errors surfaced to the CLI from the model/backend boundary.
#[derive(Debug)]
pub enum ModelError {
    Open(String),
    Load(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Open(m) | ModelError::Load(m) => write!(f, "{m}"),
        }
    }
}

impl Wave {
    /// Build a domain model from an already-opened backend.
    pub fn from_backend(backend: Box<dyn WaveformBackend>) -> Wave {
        let (signals, raw_var_count, type_counts) = build_signal_table(backend.as_ref());
        let n = signals.len();
        Wave {
            backend,
            signals,
            raw_var_count,
            type_counts,
            traces: (0..n).map(|_| None).collect(),
        }
    }

    /// Convenience constructor using the default (wellen) backend.
    pub fn open(path: &str) -> Result<Wave, ModelError> {
        use crate::backend::wellen_backend::WellenBackend;
        use crate::backend::BackendError;
        match WellenBackend::open(path) {
            Ok(b) => Ok(Wave::from_backend(Box::new(b))),
            Err(BackendError::Open(m)) => Err(ModelError::Open(m)),
            Err(BackendError::Parse(m)) => Err(ModelError::Load(m)),
        }
    }

    // -- metadata passthrough ------------------------------------------------

    pub fn path(&self) -> &str {
        self.backend.path()
    }

    pub fn ts_sec(&self) -> f64 {
        self.backend.timescale().seconds_per_tick
    }

    pub fn timescale_str(&self) -> String {
        self.backend.timescale().display
    }

    pub fn file_format(&self) -> FileFormat {
        self.backend.file_format()
    }

    pub fn date(&self) -> String {
        self.backend.date().to_string()
    }

    pub fn version(&self) -> String {
        self.backend.version().to_string()
    }

    pub fn comments(&self) -> Vec<String> {
        self.backend.comments()
    }

    pub fn raw_var_count(&self) -> usize {
        self.raw_var_count
    }

    /// Var-type counts, already sorted by descending count then type name.
    pub fn type_counts_sorted(&self) -> &[(String, usize)] {
        &self.type_counts
    }

    pub fn signals(&self) -> &[SignalInfo] {
        &self.signals
    }

    pub fn signal(&self, sid: Sid) -> &SignalInfo {
        &self.signals[sid]
    }

    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }

    /// Sorted set of all parent-scope paths across all signals.
    pub fn scopes(&self) -> Vec<String> {
        let mut set: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for s in &self.signals {
            for sc in &s.scopes {
                if !sc.is_empty() {
                    set.insert(sc.as_str());
                }
            }
        }
        set.into_iter().map(|s| s.to_string()).collect()
    }

    pub fn time_range(&self) -> Option<(i64, i64)> {
        self.backend.time_range()
    }

    pub fn time_table_len(&self) -> usize {
        self.backend.time_step_count()
    }

    // -- trace loading -------------------------------------------------------

    /// Ensure the given signals' traces are decoded and cached. Idempotent;
    /// only the not-yet-cached signals are requested from the backend, and the
    /// backend decodes each underlying signal once even across alias `Sid`s.
    pub fn ensure_loaded(&mut self, sids: &[Sid]) {
        // Collect the distinct backend handles we still need, remembering which
        // domain Sids map to each, so aliases share one decode.
        let mut need_backend: Vec<BackendSid> = Vec::new();
        let mut backend_to_sids: HashMap<BackendSid, Vec<Sid>> = HashMap::new();
        for &sid in sids {
            if self.traces[sid].is_some() {
                continue;
            }
            let bsid = self.signals[sid].backend_sid;
            backend_to_sids.entry(bsid).or_default().push(sid);
            if backend_to_sids[&bsid].len() == 1 {
                need_backend.push(bsid);
            }
        }
        if need_backend.is_empty() {
            return;
        }
        let decoded = self.backend.load_traces(&need_backend);
        for (bsid, trace) in need_backend.into_iter().zip(decoded.into_iter()) {
            let targets = &backend_to_sids[&bsid];
            if targets.len() == 1 {
                self.traces[targets[0]] = Some(trace);
            } else {
                // Multiple aliases share this trace; clone for all but the last.
                for &sid in &targets[1..] {
                    self.traces[sid] = Some(clone_trace(&trace));
                }
                self.traces[targets[0]] = Some(trace);
            }
        }
    }

    /// Load every signal's trace (used by whole-file scans). Prefer the
    /// batched/streaming methods below for whole-file work on large files, as
    /// this holds every signal's full history in memory at once.
    pub fn ensure_all_loaded(&mut self) {
        let all: Vec<Sid> = (0..self.signals.len()).collect();
        self.ensure_loaded(&all);
    }

    /// Drop the cached traces for the given signals, freeing their memory.
    pub fn release_traces(&mut self, sids: &[Sid]) {
        for &sid in sids {
            self.traces[sid] = None;
        }
    }

    /// Number of signals whose trace is currently resident (for diagnostics).
    pub fn resident_trace_count(&self) -> usize {
        self.traces.iter().filter(|t| t.is_some()).count()
    }

    /// Process the selected signals (or all) **one batch at a time**, bounding
    /// peak memory: each batch's traces are decoded, handed to `f` as a slice
    /// of `(sid, &SignalTrace)`, then released before the next batch. This is
    /// the right primitive for per-signal-independent work (summary stats,
    /// point snapshots), where holding every signal's full history at once
    /// would be wasteful or impossible on large files.
    ///
    /// `batch` is the number of signals decoded per step. Signals already
    /// resident are reused and not released (the caller owns their lifetime).
    pub fn for_each_signal_batched<F>(&mut self, sids: Option<&[Sid]>, batch: usize, mut f: F)
    where
        F: FnMut(Sid, &SignalTrace),
    {
        let batch = batch.max(1);
        let all: Vec<Sid> = match sids {
            Some(s) => s.to_vec(),
            None => (0..self.signals.len()).collect(),
        };
        let mut i = 0;
        while i < all.len() {
            let end = (i + batch).min(all.len());
            let chunk = &all[i..end];

            // Track which signals we load here so we can release exactly those
            // (never evicting traces the caller had already pinned).
            let preloaded: Vec<bool> = chunk.iter().map(|&s| self.traces[s].is_some()).collect();
            self.ensure_loaded(chunk);

            for &sid in chunk {
                if let Some(tr) = self.traces[sid].as_ref() {
                    f(sid, tr);
                }
            }

            // Release the traces this batch introduced.
            for (k, &sid) in chunk.iter().enumerate() {
                if !preloaded[k] {
                    self.traces[sid] = None;
                }
            }
            i = end;
        }
    }

    #[inline]
    fn trace(&self, sid: Sid) -> Option<&SignalTrace> {
        self.traces[sid].as_ref()
    }

    // -- replay --------------------------------------------------------------

    /// Replay value changes for the given signals within `[t0, t1]` (inclusive;
    /// `t1 = None` = unbounded), invoking `f(tick, sid, value)` in
    /// non-decreasing tick order. Within one tick, signals are emitted in
    /// declaration (writer) order. `sids = None` means all signals.
    ///
    /// Requires the relevant signals to have been [`ensure_loaded`]; any signal
    /// without a cached trace is skipped.
    ///
    /// Implemented as a binary-min-heap k-way merge: `O(n log k)`.
    pub fn for_each_event<F: FnMut(i64, Sid, ValueRef)>(
        &self,
        t0: i64,
        t1: Option<i64>,
        sids: Option<&[Sid]>,
        mut f: F,
    ) {
        let mut heap: BinaryHeap<HeapEntry> = match sids {
            Some(s) => BinaryHeap::with_capacity(s.len()),
            None => BinaryHeap::with_capacity(self.signals.len()),
        };

        // Seed the heap with each selected signal's first change.
        let seed = |sid: Sid, heap: &mut BinaryHeap<HeapEntry>| {
            if let Some(tr) = self.trace(sid) {
                if !tr.is_empty() {
                    heap.push(HeapEntry {
                        tick: tr.times[0],
                        decl_order: self.signals[sid].decl_order,
                        sid,
                        pos: 0,
                    });
                }
            }
        };
        match sids {
            Some(s) => {
                for &sid in s {
                    seed(sid, &mut heap);
                }
            }
            None => {
                for sid in 0..self.signals.len() {
                    seed(sid, &mut heap);
                }
            }
        }

        while let Some(entry) = heap.pop() {
            let tick = entry.tick;
            // Upper bound: once the smallest remaining tick exceeds t1, stop.
            if let Some(t1) = t1 {
                if tick > t1 {
                    break;
                }
            }
            let sid = entry.sid;
            let tr = self.trace(sid).unwrap();

            // Emit if within the lower bound.
            if tick >= t0 {
                f(tick, sid, ValueRef::from_raw(&tr.values[entry.pos]));
            }

            // Advance this signal's cursor and re-heap.
            let next = entry.pos + 1;
            if next < tr.times.len() {
                heap.push(HeapEntry {
                    tick: tr.times[next],
                    decl_order: entry.decl_order,
                    sid,
                    pos: next,
                });
            }
        }
    }

    /// Collect the earliest events (in `[t0, t1]`) across the selected signals,
    /// **memory-bounded**: signals are decoded in batches and released, and only
    /// the smallest `limit` events are retained (via a bounded max-heap). This
    /// lets `dump` run on files far too large to hold every signal's full
    /// history at once. `limit == 0` means unlimited; in that case this loads
    /// in batches but must retain all in-range events (the caller is asking for
    /// the whole stream and pays the memory for the output it requested).
    ///
    /// Returns `(events, total_in_range, truncated)` where `events` is sorted by
    /// (tick, declaration order) and contains at most `limit` entries.
    /// `total_in_range` counts every in-range event scanned; `truncated` is
    /// true iff more than `limit` in-range events exist.
    pub fn collect_events_bounded(
        &mut self,
        t0: i64,
        t1: Option<i64>,
        sids: Option<&[Sid]>,
        limit: usize,
        batch: usize,
    ) -> (Vec<DumpEvent>, usize, bool) {
        // Bounded max-heap: holds the smallest `limit` events seen. The heap's
        // top is the *largest* retained event, so a new event smaller than the
        // top evicts it. Ordering key is (tick, decl_order) ascending, so the
        // "largest" under that key sits at the max-heap root.
        let mut keep: BinaryHeap<BoundedEvent> = BinaryHeap::new();
        let mut all: Vec<DumpEvent> = Vec::new(); // used only when unlimited
        let mut total = 0usize;

        // Precompute declaration order per signal so the bounded-heap eviction
        // can compare events by their true (tick, decl_order) key *during*
        // collection. Resolving it afterward would make eviction compare equal
        // keys and retain the wrong subset. This is a cheap O(signals) copy.
        let decl_order: Vec<usize> = self.signals.iter().map(|s| s.decl_order).collect();

        self.for_each_signal_batched(sids, batch, |sid, tr| {
            // Window bounds within this signal's trace.
            let lo = lower_bound(&tr.times, t0);
            let hi = match t1 {
                Some(t1) => upper_bound(&tr.times, t1),
                None => tr.times.len(),
            };
            let dord = decl_order[sid];
            for i in lo..hi {
                total += 1;
                let ev = DumpEvent {
                    tick: tr.times[i],
                    sid,
                    decl_order: dord,
                    value: owned_from_raw(&tr.values[i]),
                };
                if limit == 0 {
                    all.push(ev);
                } else if keep.len() < limit {
                    keep.push(BoundedEvent(ev));
                } else if let Some(top) = keep.peek() {
                    // Evict the current largest if this event is smaller.
                    if event_less(&ev, &top.0) {
                        keep.pop();
                        keep.push(BoundedEvent(ev));
                    }
                }
            }
        });

        // Sort the retained events by (tick, declaration order).
        let mut events: Vec<DumpEvent> = if limit == 0 {
            all
        } else {
            keep.into_iter().map(|b| b.0).collect()
        };
        events.sort_by(|a, b| {
            a.tick
                .cmp(&b.tick)
                .then_with(|| a.decl_order.cmp(&b.decl_order))
        });
        let truncated = limit != 0 && total > limit;
        (events, total, truncated)
    }

    /// Last-known values at or before `t_at` for the given signals (or all).
    /// Returns only signals that have a known value by `t_at`.
    pub fn snapshot(&self, t_at: i64, sids: Option<&[Sid]>) -> HashMap<Sid, OwnedValue> {
        // A snapshot needs only the last change at-or-before t_at per signal,
        // which is a per-signal binary search — no global merge required. This
        // is both simpler and faster than replaying every event.
        let mut state: HashMap<Sid, OwnedValue> = HashMap::new();
        self.for_selected(sids, |sid| {
            if let Some(tr) = self.trace(sid) {
                if let Some(pos) = last_at_or_before(&tr.times, t_at) {
                    state.insert(sid, owned_from_raw(&tr.values[pos]));
                }
            }
        });
        state
    }

    /// Two snapshots at `ta` and `tb` (`ta <= tb`) via per-signal binary search.
    pub fn snapshot_pair(
        &self,
        ta: i64,
        tb: i64,
        sids: Option<&[Sid]>,
    ) -> (HashMap<Sid, OwnedValue>, HashMap<Sid, OwnedValue>) {
        let mut a: HashMap<Sid, OwnedValue> = HashMap::new();
        let mut b: HashMap<Sid, OwnedValue> = HashMap::new();
        self.for_selected(sids, |sid| {
            if let Some(tr) = self.trace(sid) {
                if let Some(pos) = last_at_or_before(&tr.times, ta) {
                    a.insert(sid, owned_from_raw(&tr.values[pos]));
                }
                if let Some(pos) = last_at_or_before(&tr.times, tb) {
                    b.insert(sid, owned_from_raw(&tr.values[pos]));
                }
            }
        });
        (a, b)
    }

    /// Memory-bounded snapshot: like [`snapshot`], but decodes signals in
    /// batches and releases each batch's traces immediately, so peak memory is
    /// proportional to one batch rather than the whole file. Use this for
    /// whole-file (unfiltered) snapshots on large inputs.
    pub fn snapshot_streaming(
        &mut self,
        t_at: i64,
        sids: Option<&[Sid]>,
        batch: usize,
    ) -> HashMap<Sid, OwnedValue> {
        let mut state: HashMap<Sid, OwnedValue> = HashMap::new();
        self.for_each_signal_batched(sids, batch, |sid, tr| {
            if let Some(pos) = last_at_or_before(&tr.times, t_at) {
                state.insert(sid, owned_from_raw(&tr.values[pos]));
            }
        });
        state
    }

    /// Memory-bounded pair snapshot (see [`snapshot_streaming`]).
    pub fn snapshot_pair_streaming(
        &mut self,
        ta: i64,
        tb: i64,
        sids: Option<&[Sid]>,
        batch: usize,
    ) -> (HashMap<Sid, OwnedValue>, HashMap<Sid, OwnedValue>) {
        let mut a: HashMap<Sid, OwnedValue> = HashMap::new();
        let mut b: HashMap<Sid, OwnedValue> = HashMap::new();
        self.for_each_signal_batched(sids, batch, |sid, tr| {
            if let Some(pos) = last_at_or_before(&tr.times, ta) {
                a.insert(sid, owned_from_raw(&tr.values[pos]));
            }
            if let Some(pos) = last_at_or_before(&tr.times, tb) {
                b.insert(sid, owned_from_raw(&tr.values[pos]));
            }
        });
        (a, b)
    }

    /// Run `f(sid)` over the selected signals (or all if `None`).
    #[inline]
    fn for_selected<F: FnMut(Sid)>(&self, sids: Option<&[Sid]>, mut f: F) {
        match sids {
            Some(s) => {
                for &sid in s {
                    f(sid);
                }
            }
            None => {
                for sid in 0..self.signals.len() {
                    f(sid);
                }
            }
        }
    }
}

/// Convert a borrowed [`RawValue`] to an [`OwnedValue`] (canonical strings).
fn owned_from_raw(v: &RawValue) -> OwnedValue {
    match v {
        RawValue::Bits(s) => OwnedValue::Bits(s.clone()),
        RawValue::Real(r) => OwnedValue::Real(fmt_real(*r)),
        RawValue::Str(s) => OwnedValue::Str(s.clone()),
        RawValue::Event => OwnedValue::Event,
    }
}

fn clone_trace(t: &SignalTrace) -> SignalTrace {
    SignalTrace {
        times: t.times.clone(),
        values: t.values.clone(),
    }
}

/// Index of the last change at or before `t` via binary search, or `None` if
/// the first change is after `t`.
#[inline]
fn last_at_or_before(times: &[i64], t: i64) -> Option<usize> {
    if times.is_empty() || times[0] > t {
        return None;
    }
    // partition_point returns the count of elements <= t; the last such index
    // is that count - 1.
    let count = times.partition_point(|&x| x <= t);
    if count == 0 {
        None
    } else {
        Some(count - 1)
    }
}

/// First index whose time is `>= t` (lower bound).
#[inline]
fn lower_bound(times: &[i64], t: i64) -> usize {
    times.partition_point(|&x| x < t)
}

/// Count of elements `<= t` (exclusive upper-bound index for an inclusive
/// window ending at `t`).
#[inline]
fn upper_bound(times: &[i64], t: i64) -> usize {
    times.partition_point(|&x| x <= t)
}

/// One emitted value-change event with an owned value, used by the bounded
/// dump collector.
pub struct DumpEvent {
    pub tick: i64,
    pub sid: Sid,
    pub decl_order: usize,
    pub value: OwnedValue,
}

/// Ascending order on (tick, declaration order). `true` if `a` precedes `b`.
#[inline]
fn event_less(a: &DumpEvent, b: &DumpEvent) -> bool {
    (a.tick, a.decl_order) < (b.tick, b.decl_order)
}

/// Wrapper giving a *max-heap* on (tick, decl_order): the largest retained
/// event sits at the root so it can be evicted when a smaller one arrives.
struct BoundedEvent(DumpEvent);

impl PartialEq for BoundedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.0.tick == other.0.tick && self.0.decl_order == other.0.decl_order
    }
}
impl Eq for BoundedEvent {}
impl Ord for BoundedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Natural ascending order so BinaryHeap (a max-heap) keeps the largest
        // (tick, decl_order) at the root.
        (self.0.tick, self.0.decl_order).cmp(&(other.0.tick, other.0.decl_order))
    }
}
impl PartialOrd for BoundedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Min-heap entry for the k-way merge. `BinaryHeap` is a max-heap, so `Ord` is
/// reversed: the entry that should come out first (smallest tick, then smallest
/// declaration order) must compare as *greatest*.
struct HeapEntry {
    tick: i64,
    decl_order: usize,
    sid: Sid,
    pos: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.tick == other.tick && self.decl_order == other.decl_order
    }
}
impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse so the heap yields the minimum (tick, decl_order) first.
        other
            .tick
            .cmp(&self.tick)
            .then_with(|| other.decl_order.cmp(&self.decl_order))
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Build the domain signal table from the backend's variable declarations.
/// Variables sharing a backend handle (aliases) merge into one entry; the
/// table is sorted by canonical path and assigned dense ids.
fn build_signal_table(
    backend: &dyn WaveformBackend,
) -> (Vec<SignalInfo>, usize, Vec<(String, usize)>) {
    struct Group {
        width: u32,
        type_str: &'static str,
        kind: ValueKind,
        backend_sid: BackendSid,
        paths: Vec<String>,
        scopes: Vec<String>,
        decl_order: usize,
    }

    let mut groups: HashMap<BackendSid, Group> = HashMap::new();
    let mut raw_var_count = 0usize;
    let mut type_counts: HashMap<&'static str, usize> = HashMap::new();

    for (decl_idx, decl) in backend.var_decls().into_iter().enumerate() {
        raw_var_count += 1;
        *type_counts.entry(decl.type_str).or_insert(0) += 1;

        let g = groups.entry(decl.backend_sid).or_insert_with(|| Group {
            width: decl.width,
            type_str: decl.type_str,
            kind: decl.kind,
            backend_sid: decl.backend_sid,
            paths: Vec::new(),
            scopes: Vec::new(),
            decl_order: decl_idx,
        });
        if decl_idx < g.decl_order {
            g.decl_order = decl_idx;
        }
        g.paths.push(decl.full_path);
        if !decl.scope_path.is_empty() && !g.scopes.contains(&decl.scope_path) {
            g.scopes.push(decl.scope_path);
        }
    }

    let mut infos: Vec<SignalInfo> = groups
        .into_values()
        .map(|mut g| {
            g.paths.sort();
            g.paths.dedup();
            let path = g.paths[0].clone();
            SignalInfo {
                path,
                aliases: g.paths,
                width: g.width,
                type_str: g.type_str,
                kind: g.kind,
                scopes: g.scopes,
                decl_order: g.decl_order,
                backend_sid: g.backend_sid,
            }
        })
        .collect();

    infos.sort_by(|a, b| a.path.cmp(&b.path));

    // Sort type counts by descending count, then name, for stable `info` output.
    let mut counts: Vec<(String, usize)> =
        type_counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    (infos, raw_var_count, counts)
}
