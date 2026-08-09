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
    /// Parent scope path of each alias, index-aligned with [`aliases`]; a
    /// top-level alias stores `""`. Kept per alias rather than de-duplicated
    /// because hierarchy-aware selection (`--scope`, `--depth`) and leaf-name
    /// matching all need the scope *of the path being judged* — and the leaf
    /// can only be split off a path with its own scope, never by searching for
    /// the last separator (escaped identifiers contain dots).
    pub alias_scopes: Vec<String>,
    /// Smallest declaration index among aliases; ties timestamp-coincident
    /// events back to writer order during replay.
    pub decl_order: usize,
    /// Opaque backend handle used to request this signal's trace.
    backend_sid: BackendSid,
}

impl SignalInfo {
    /// Each alias as `(path, scope_path)`. The scope is `""` for a top-level
    /// alias. Pair up rather than iterating `aliases` alone whenever the leaf
    /// name or the hierarchy position matters.
    pub fn alias_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.aliases
            .iter()
            .zip(self.alias_scopes.iter())
            .map(|(p, s)| (p.as_str(), s.as_str()))
    }

    /// Does any alias equal `pattern_lower` (which must already be lower-cased)?
    /// Used for the exact-full-path lookups that bypass pattern matching.
    pub fn has_exact_path_ci(&self, pattern_lower: &str) -> bool {
        self.aliases.iter().any(|p| p.to_lowercase() == pattern_lower)
    }
}

/// Build a bare [`SignalInfo`] from `(path, scope)` pairs, for tests that
/// exercise alias handling without opening a waveform.
#[cfg(test)]
pub(crate) fn test_signal(aliases: &[(&str, &str)]) -> SignalInfo {
    SignalInfo {
        path: aliases[0].0.to_string(),
        aliases: aliases.iter().map(|(p, _)| p.to_string()).collect(),
        width: 1,
        type_str: "wire",
        kind: ValueKind::Bits,
        alias_scopes: aliases.iter().map(|(_, s)| s.to_string()).collect(),
        decl_order: 0,
        backend_sid: crate::backend::BackendSid(0),
    }
}

/// The leaf (local variable name) of `path`, given the scope path it sits in.
///
/// Derived structurally — never by searching `path` for the last separator. A
/// VCD escaped identifier may itself contain dots (`tb.\foo.bar` is the signal
/// `\foo.bar` in scope `tb`), so only the scope's own length says where the
/// name begins. A vector's range suffix is folded into the path but not into
/// the declared name, so the leaf carries it (`data[7:0]`) — which is what a
/// user matching on `data` or `data[7:0]` expects either way.
pub fn leaf_of<'a>(path: &'a str, scope: &str) -> &'a str {
    if scope.is_empty() {
        return path;
    }
    // Skip the scope and the one separator byte between it and the name. Guard
    // against a backend whose scope is not actually a prefix of the path.
    match path.get(scope.len() + 1..) {
        Some(leaf) if path.as_bytes().get(scope.len()).is_some_and(|b| *b == b'.' || *b == b'/') => {
            leaf
        }
        _ => path,
    }
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

/// File extensions [`WellenBackend`] handles directly. Any extension
/// outside this set is routed to the plugin loader; an empty extension
/// (no dot in the filename) also goes to wellen, which auto-detects by
/// magic bytes.
const BUILT_IN_EXTENSIONS: &[&str] = &["vcd", "fst", "ghw"];

fn extract_extension(path: &str) -> String {
    // Use Path::extension so dots inside directory names (e.g.
    // "dir.with.dot/data") don't confuse the dispatcher.
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_built_in_extension(ext: &str) -> bool {
    BUILT_IN_EXTENSIONS.iter().any(|b| *b == ext)
}

#[cfg(test)]
mod open_dispatch_tests {
    //! Verify `Wave::open`'s extension-based dispatch logic at the
    //! pure-function level — `extract_extension` and
    //! `is_built_in_extension`. Behavioural tests against actual files
    //! live in `tests/` and the runtime smoke checks in `verify/`.

    use super::*;

    #[test]
    fn extract_lowercase_normalises_uppercase() {
        assert_eq!(extract_extension("data.FOO"), "foo");
        assert_eq!(extract_extension("data.Foo"), "foo");
    }

    #[test]
    fn extract_uses_last_extension_for_multi_dot_filenames() {
        assert_eq!(extract_extension("archive.tar.gz"), "gz");
        assert_eq!(extract_extension("v1.0.data.foo"), "foo");
    }

    #[test]
    fn extract_handles_directory_dots() {
        // Without Path-aware extraction, this used to (mis)parse as a
        // "with/data" extension. Now: file is "data", no extension.
        assert_eq!(extract_extension("dir.with.dot/data"), "");
        assert_eq!(extract_extension("dir.with.dot/data.foo"), "foo");
        assert_eq!(extract_extension("/abs.path/here/file.fst"), "fst");
    }

    #[test]
    fn extract_no_extension_returns_empty() {
        assert_eq!(extract_extension("data"), "");
        assert_eq!(extract_extension(""), "");
        assert_eq!(extract_extension("/some/path"), "");
    }

    #[test]
    fn extract_hidden_file_with_no_real_extension() {
        // Standard convention: ".foo" is a hidden file with no extension.
        assert_eq!(extract_extension(".foo"), "");
        assert_eq!(extract_extension("dir/.gitignore"), "");
    }

    #[test]
    fn extract_trailing_dot_yields_empty() {
        // Path::extension on "foo." returns Some("") — still routes
        // to wellen because is_built_in_extension("") is false and
        // the dispatcher treats empty as "no plugin to ask".
        assert_eq!(extract_extension("foo."), "");
    }

    #[test]
    fn extract_dot_only_filename_yields_empty() {
        // "." and ".." aren't real files but shouldn't blow up.
        assert_eq!(extract_extension("."), "");
        assert_eq!(extract_extension(".."), "");
    }

    #[test]
    fn built_in_recognises_exact_lowercase() {
        for b in BUILT_IN_EXTENSIONS {
            assert!(is_built_in_extension(b));
        }
        assert!(!is_built_in_extension("foo"));
        assert!(!is_built_in_extension(""));
        assert!(!is_built_in_extension("vcd2"));
        assert!(!is_built_in_extension("xvcd"));
    }
}

#[cfg(test)]
mod leaf_tests {
    //! `leaf_of` splits a path at its scope, which is the only correct way:
    //! the separator search a reader expects (`rsplit('.')`) is wrong for the
    //! escaped identifiers real VCDs contain.

    use super::leaf_of;

    #[test]
    fn plain_path_splits_at_its_scope() {
        assert_eq!(leaf_of("top.u_dma.req", "top.u_dma"), "req");
        assert_eq!(leaf_of("top.status", "top"), "status");
    }

    #[test]
    fn top_level_signal_is_its_own_leaf() {
        assert_eq!(leaf_of("clk", ""), "clk");
    }

    #[test]
    fn escaped_identifier_keeps_its_dots() {
        // verify/fixtures/escaped_trace.vcd: `\foo.bar` declared in scope `tb`.
        // rsplit('.') would answer "bar" and lose half the name.
        assert_eq!(leaf_of(r"tb.\foo.bar", "tb"), r"\foo.bar");
    }

    #[test]
    fn vector_range_travels_with_the_leaf() {
        // The range is folded into the path, not the declared name, so the leaf
        // carries it — `data` and `data[7:0]` both match it as a substring.
        assert_eq!(leaf_of("tb.data[7:0]", "tb"), "data[7:0]");
    }

    #[test]
    fn slash_separated_hierarchy_splits_too() {
        // The built-in FSDB backend emits '/' as a hierarchy separator.
        assert_eq!(leaf_of("top/u_dma/req", "top/u_dma"), "req");
    }

    #[test]
    fn a_scope_that_is_not_a_prefix_yields_the_whole_path() {
        // Defensive: a backend breaking the path/scope contract must not panic
        // or slice mid-character.
        assert_eq!(leaf_of("top.req", "other"), "top.req");
        assert_eq!(leaf_of("ab", "abcdef"), "ab");
        assert_eq!(leaf_of("tb·x", "tb"), "tb·x");
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

    /// Open a file, dispatching by file extension:
    /// * `.vcd` / `.fst` / `.ghw` (or no extension) → built-in `wellen` backend.
    /// * `.wlf` / `.fsdb` → compiled-in built-in backend (linux-x86_64 builds).
    /// * any other extension `<ext>` → external backend named by
    ///   `$RWAVE_PLUGIN_<EXT>` (see `docs/PLUGIN.md`).
    ///
    /// `$RWAVE_PLUGIN_<EXT>` also overrides a built-in of the same extension
    /// (e.g. an external `.fsdb` backend superseding the built-in NPI one). When
    /// nothing handles the extension, the error names the env var to set.
    pub fn open(path: &str) -> Result<Wave, ModelError> {
        use crate::backend::plugin_backend::PluginBackend;
        use crate::backend::wellen_backend::WellenBackend;
        use crate::backend::BackendError;

        let ext = extract_extension(path);
        if !ext.is_empty() && !is_built_in_extension(&ext) {
            match PluginBackend::open(path, &ext) {
                Ok(b) => return Ok(Wave::from_backend(Box::new(b))),
                Err(BackendError::Open(m)) => return Err(ModelError::Open(m)),
                Err(BackendError::Parse(m)) => return Err(ModelError::Load(m)),
            }
        }

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
            for sc in &s.alias_scopes {
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

    /// Whether the backend can decode a time window meaningfully faster than a
    /// full history (i.e. it can seek by time); when false, callers behave
    /// exactly as before. FST and the built-in FSDB and WLF backends report
    /// true; VCD and GHW cannot seek.
    ///
    /// Only `snapshot` and `compare` consult this to prefer the windowed
    /// collector. `dump` reaches it only via `collect_events_bounded`, i.e.
    /// when a selection exceeds `STREAMING_SIGNAL_THRESHOLD`; `summary` and
    /// `search` still decode full histories through `ensure_loaded`.
    pub fn supports_windowed(&self) -> bool {
        self.backend.supports_windowed()
    }

    /// Design-connectivity queries, when the backend can answer them.
    ///
    /// `None` for every waveform-only backend — VCD/FST/GHW, WLF, and every
    /// external plugin (including an FFR-based `.fsdb` plugin selected through
    /// `$RWAVE_PLUGIN_FSDB`, whose reader API has no connectivity calls at all).
    /// Only `trace` consults this, and it turns `None` into a clean
    /// "unsupported" message rather than a partial answer.
    pub fn design_query(&mut self) -> Option<&mut dyn crate::backend::DesignQuery> {
        self.backend.design_query()
    }

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

    /// Like [`for_each_signal_batched`](Self::for_each_signal_batched), but for
    /// queries confined to `[from, to]`: when the backend can seek by time
    /// ([`supports_windowed`](Self::supports_windowed)), it decodes only each
    /// signal's value entering the window plus the in-window changes, avoiding
    /// a full-history scan. Otherwise it falls back to the full batched decode,
    /// so behavior is identical for backends that don't specialize.
    ///
    /// The windowed traces handed to `f` are **partial** and never enter the
    /// trace cache: `f` must derive its answer from `[from, to]` alone (which
    /// is exactly what the point/window commands do). `to = None` = unbounded.
    fn for_each_signal_windowed<F>(
        &mut self,
        sids: Option<&[Sid]>,
        from: i64,
        to: Option<i64>,
        batch: usize,
        mut f: F,
    ) where
        F: FnMut(Sid, &SignalTrace),
    {
        // No by-time seek available: identical to the full batched path.
        if !self.backend.supports_windowed() {
            self.for_each_signal_batched(sids, batch, f);
            return;
        }

        let batch = batch.max(1);
        let all: Vec<Sid> = match sids {
            Some(s) => s.to_vec(),
            None => (0..self.signals.len()).collect(),
        };
        let mut i = 0;
        while i < all.len() {
            let end = (i + batch).min(all.len());
            let chunk = &all[i..end];

            // Distinct backend handles for this batch, remembering which Sids
            // map to each so aliases share one windowed decode. Unlike
            // `ensure_loaded`, this ignores the resident cache: windowed traces
            // are partial and must stay out of it.
            let mut need_backend: Vec<BackendSid> = Vec::new();
            let mut backend_to_sids: HashMap<BackendSid, Vec<Sid>> = HashMap::new();
            for &sid in chunk {
                let bsid = self.signals[sid].backend_sid;
                let entry = backend_to_sids.entry(bsid).or_default();
                entry.push(sid);
                if entry.len() == 1 {
                    need_backend.push(bsid);
                }
            }

            let decoded = self.backend.load_traces_windowed(&need_backend, from, to);
            for (bsid, trace) in need_backend.iter().zip(decoded.iter()) {
                for &sid in &backend_to_sids[bsid] {
                    f(sid, trace);
                }
            }
            // `decoded` drops here — partial traces are never cached.
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
    pub fn for_each_event<F: FnMut(i64, Sid, &RawValue)>(
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
                f(tick, sid, &tr.values[entry.pos]);
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

        self.for_each_signal_windowed(sids, t0, t1, batch, |sid, tr| {
            // Window bounds within this signal's trace. On a windowed decode the
            // trace already covers only `[t0, t1]` (plus the pre-window seed,
            // which `lower_bound(t0)` skips), so these bounds pick out exactly
            // the same in-range events as a full-trace scan would.
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
                    value: tr.values[i].clone(),
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
    pub fn snapshot(&self, t_at: i64, sids: Option<&[Sid]>) -> HashMap<Sid, RawValue> {
        // A snapshot needs only the last change at-or-before t_at per signal,
        // which is a per-signal binary search — no global merge required. This
        // is both simpler and faster than replaying every event.
        let mut state: HashMap<Sid, RawValue> = HashMap::new();
        self.for_selected(sids, |sid| {
            if let Some(tr) = self.trace(sid) {
                if let Some(pos) = last_at_or_before(&tr.times, t_at) {
                    state.insert(sid, tr.values[pos].clone());
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
    ) -> (HashMap<Sid, RawValue>, HashMap<Sid, RawValue>) {
        let mut a: HashMap<Sid, RawValue> = HashMap::new();
        let mut b: HashMap<Sid, RawValue> = HashMap::new();
        self.for_selected(sids, |sid| {
            if let Some(tr) = self.trace(sid) {
                if let Some(pos) = last_at_or_before(&tr.times, ta) {
                    a.insert(sid, tr.values[pos].clone());
                }
                if let Some(pos) = last_at_or_before(&tr.times, tb) {
                    b.insert(sid, tr.values[pos].clone());
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
    ) -> HashMap<Sid, RawValue> {
        let mut state: HashMap<Sid, RawValue> = HashMap::new();
        // A point query: the window collapses to `t_at`, so a seeking backend
        // reads just the value in effect at `t_at` per signal.
        self.for_each_signal_windowed(sids, t_at, Some(t_at), batch, |sid, tr| {
            if let Some(pos) = last_at_or_before(&tr.times, t_at) {
                state.insert(sid, tr.values[pos].clone());
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
    ) -> (HashMap<Sid, RawValue>, HashMap<Sid, RawValue>) {
        let mut a: HashMap<Sid, RawValue> = HashMap::new();
        let mut b: HashMap<Sid, RawValue> = HashMap::new();
        // On a seeking backend, two point windows beat one spanning window:
        // window cost grows with the span, so a far-apart pair would re-read
        // everything between the instants. Without seeking, the split would
        // double the full-decode fallback, so the one-window path stays for
        // that case; its seed and in-window changes answer both instants
        // from the single pass.
        if self.backend.supports_windowed() && ta != tb {
            self.for_each_signal_windowed(sids, ta, Some(ta), batch, |sid, tr| {
                if let Some(pos) = last_at_or_before(&tr.times, ta) {
                    a.insert(sid, tr.values[pos].clone());
                }
            });
            self.for_each_signal_windowed(sids, tb, Some(tb), batch, |sid, tr| {
                if let Some(pos) = last_at_or_before(&tr.times, tb) {
                    b.insert(sid, tr.values[pos].clone());
                }
            });
            return (a, b);
        }
        // `ta <= tb`, so one window `[ta, tb]` carries both answers: the seed
        // (last change <= ta) resolves `ta`, and the last change <= tb resolves
        // `tb`.
        self.for_each_signal_windowed(sids, ta, Some(tb), batch, |sid, tr| {
            if let Some(pos) = last_at_or_before(&tr.times, ta) {
                a.insert(sid, tr.values[pos].clone());
            }
            if let Some(pos) = last_at_or_before(&tr.times, tb) {
                b.insert(sid, tr.values[pos].clone());
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
    pub value: RawValue,
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
    use rustc_hash::FxHashMap;

    struct Group {
        width: u32,
        type_str: &'static str,
        kind: ValueKind,
        backend_sid: BackendSid,
        /// `(full_path, scope_path)` per declaration, kept paired: the scope is
        /// what later splits the leaf name off its path.
        paths: Vec<(String, String)>,
        decl_order: usize,
    }

    let decls = backend.var_decls();
    let raw_var_count = decls.len();

    // Group declarations by backend handle. Most hierarchies have far fewer
    // aliased signals than total var-decls, but reserving for the upper bound
    // avoids rehashing on the way there. FxHashMap (vs the default SipHash map)
    // is markedly faster for the many integer-keyed inserts here.
    let mut groups: FxHashMap<BackendSid, Group> =
        FxHashMap::with_capacity_and_hasher(decls.len(), Default::default());
    let mut type_counts: FxHashMap<&'static str, usize> = FxHashMap::default();

    for (decl_idx, decl) in decls.into_iter().enumerate() {
        *type_counts.entry(decl.type_str).or_insert(0) += 1;

        match groups.get_mut(&decl.backend_sid) {
            Some(g) => {
                // Existing group (an alias): keep the earliest declaration index
                // and accumulate the path / scope.
                if decl_idx < g.decl_order {
                    g.decl_order = decl_idx;
                }
                g.paths.push((decl.full_path, decl.scope_path));
            }
            None => {
                let mut paths = Vec::with_capacity(1);
                paths.push((decl.full_path, decl.scope_path));
                groups.insert(
                    decl.backend_sid,
                    Group {
                        width: decl.width,
                        type_str: decl.type_str,
                        kind: decl.kind,
                        backend_sid: decl.backend_sid,
                        paths,
                        decl_order: decl_idx,
                    },
                );
            }
        }
    }

    let mut infos: Vec<SignalInfo> = Vec::with_capacity(groups.len());
    for mut g in groups.into_values() {
        // The vast majority of signals have a single path; only pay the
        // sort/dedup when there is more than one alias. Sorting and de-duping
        // by path alone keeps the two output vectors index-aligned; one path
        // always carries one scope, so the discarded duplicates are identical.
        if g.paths.len() > 1 {
            g.paths.sort_by(|a, b| a.0.cmp(&b.0));
            g.paths.dedup_by(|a, b| a.0 == b.0);
        }
        let path = g.paths[0].0.clone();
        let (aliases, alias_scopes): (Vec<String>, Vec<String>) = g.paths.into_iter().unzip();
        infos.push(SignalInfo {
            path,
            aliases,
            width: g.width,
            type_str: g.type_str,
            kind: g.kind,
            alias_scopes,
            decl_order: g.decl_order,
            backend_sid: g.backend_sid,
        });
    }

    // Signal paths are unique, so an unstable sort is correct and faster than
    // a stable one (less memory, no stability bookkeeping).
    infos.sort_unstable_by(|a, b| a.path.cmp(&b.path));

    // Sort type counts by descending count, then name, for stable `info` output.
    let mut counts: Vec<(String, usize)> =
        type_counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    (infos, raw_var_count, counts)
}

#[cfg(test)]
mod windowed_equiv_tests {
    //! The windowed (by-time-seek) collector must produce byte-identical
    //! answers to the full-history path. We drive both through one in-memory
    //! backend whose `windowed` flag toggles the seek path on and off; when on,
    //! it serves the documented window (each signal's last change at-or-before
    //! `from`, then the changes in `(from, to]`) derived from the same data the
    //! full path returns. Equality across the two therefore pins the domain
    //! wiring and the window contract that the NPI backend implements natively.

    use super::*;
    use crate::backend::{BitStr, Timescale, VarDecl};

    struct MockBackend {
        /// `(backend_sid, ascending (tick, value) changes)` per signal.
        data: Vec<(usize, Vec<(i64, RawValue)>)>,
        windowed: bool,
        /// Serve windows in the WLF backend's shape: the carried value
        /// tagged `from - 1`, a change exactly at `from` at its own tick.
        /// Consumers read the seed's value, never its time.
        wlf_seed_shape: bool,
    }

    impl MockBackend {
        fn changes(&self, bsid: usize) -> &[(i64, RawValue)] {
            self.data
                .iter()
                .find(|(s, _)| *s == bsid)
                .map(|(_, c)| c.as_slice())
                .unwrap_or(&[])
        }

        fn full_trace(&self, bsid: usize) -> SignalTrace {
            let ch = self.changes(bsid);
            SignalTrace {
                times: ch.iter().map(|(t, _)| *t).collect(),
                values: ch.iter().map(|(_, v)| v.clone()).collect(),
            }
        }

        /// Seed (last change `<= from`) followed by every change in
        /// `(from, to]` — the exact contract `load_traces_windowed` documents.
        /// With `wlf_seed_shape`, the same window in the shape the WLF
        /// backend emits: carried value at `from - 1`, at-`from` change at
        /// `from`.
        fn window_trace(&self, bsid: usize, from: i64, to: Option<i64>) -> SignalTrace {
            let ch = self.changes(bsid);
            let mut times = Vec::new();
            let mut values = Vec::new();
            if self.wlf_seed_shape {
                if let Some((_, v)) = ch.iter().rev().find(|(t, _)| *t < from) {
                    times.push(from - 1);
                    values.push(v.clone());
                }
                for (t, v) in ch {
                    if *t >= from && to.is_none_or(|hi| *t <= hi) {
                        times.push(*t);
                        values.push(v.clone());
                    }
                }
                return SignalTrace { times, values };
            }
            if let Some((t, v)) = ch.iter().rev().find(|(t, _)| *t <= from) {
                times.push(*t);
                values.push(v.clone());
            }
            for (t, v) in ch {
                if *t > from && to.is_none_or(|hi| *t <= hi) {
                    times.push(*t);
                    values.push(v.clone());
                }
            }
            SignalTrace { times, values }
        }
    }

    impl WaveformBackend for MockBackend {
        fn path(&self) -> &str {
            "mock"
        }
        fn file_format(&self) -> FileFormat {
            FileFormat::Unknown
        }
        fn timescale(&self) -> Timescale {
            Timescale {
                seconds_per_tick: 1e-9,
                display: "1ns".to_string(),
            }
        }
        fn date(&self) -> &str {
            ""
        }
        fn version(&self) -> &str {
            ""
        }
        fn comments(&self) -> Vec<String> {
            Vec::new()
        }
        fn var_decls(&self) -> Vec<VarDecl> {
            self.data
                .iter()
                .enumerate()
                .map(|(i, (bsid, _))| VarDecl {
                    full_path: format!("top.s{i}"),
                    scope_path: "top".to_string(),
                    width: 1,
                    type_str: "wire",
                    kind: ValueKind::Bits,
                    backend_sid: BackendSid(*bsid),
                })
                .collect()
        }
        fn time_range(&self) -> Option<(i64, i64)> {
            let mut lo = i64::MAX;
            let mut hi = i64::MIN;
            for (_, ch) in &self.data {
                for (t, _) in ch {
                    lo = lo.min(*t);
                    hi = hi.max(*t);
                }
            }
            if lo <= hi {
                Some((lo, hi))
            } else {
                None
            }
        }
        fn time_step_count(&self) -> usize {
            0
        }
        fn load_traces(&mut self, sids: &[BackendSid]) -> Vec<SignalTrace> {
            sids.iter().map(|s| self.full_trace(s.0)).collect()
        }
        fn supports_windowed(&self) -> bool {
            self.windowed
        }
        fn load_traces_windowed(
            &mut self,
            sids: &[BackendSid],
            from: i64,
            to: Option<i64>,
        ) -> Vec<SignalTrace> {
            sids.iter().map(|s| self.window_trace(s.0, from, to)).collect()
        }
    }

    fn bits(s: &str) -> RawValue {
        RawValue::Bits(BitStr::new(s))
    }

    fn dataset() -> Vec<(usize, Vec<(i64, RawValue)>)> {
        vec![
            // A toggling multi-change signal.
            (
                10,
                vec![
                    (0, bits("0")),
                    (5, bits("1")),
                    (12, bits("0")),
                    (20, bits("1")),
                ],
            ),
            // Different edges, first change after 0.
            (11, vec![(3, bits("1")), (8, bits("0")), (15, bits("1"))]),
            // Static signal: one change at 0.
            (12, vec![(0, bits("1"))]),
            // A signal whose first change is well past the small ticks.
            (13, vec![(18, bits("1")), (25, bits("0"))]),
        ]
    }

    fn mk(windowed: bool) -> Wave {
        Wave::from_backend(Box::new(MockBackend {
            data: dataset(),
            windowed,
            wlf_seed_shape: false,
        }))
    }

    fn mk_wlf_shape() -> Wave {
        Wave::from_backend(Box::new(MockBackend {
            data: dataset(),
            windowed: true,
            wlf_seed_shape: true,
        }))
    }

    #[test]
    fn snapshot_windowed_matches_full() {
        let mut full = mk(false);
        full.ensure_all_loaded();
        let mut win = mk(true);
        // Probe before, at, and between every edge, and past the end.
        for t in [-1, 0, 2, 3, 5, 6, 8, 12, 15, 17, 18, 20, 25, 100] {
            let a = full.snapshot(t, None);
            let b = win.snapshot_streaming(t, None, 2);
            assert_eq!(a, b, "snapshot mismatch at t={t}");
        }
    }

    #[test]
    fn compare_pair_windowed_matches_full() {
        let mut full = mk(false);
        full.ensure_all_loaded();
        let mut win = mk(true);
        for (ta, tb) in [(-1, 0), (0, 12), (3, 15), (5, 20), (8, 25), (0, 100)] {
            let (fa, fb) = full.snapshot_pair(ta, tb, None);
            let (wa, wb) = win.snapshot_pair_streaming(ta, tb, None, 2);
            assert_eq!(fa, wa, "compare-a mismatch at ta={ta}");
            assert_eq!(fb, wb, "compare-b mismatch at tb={tb}");
        }
    }

    #[test]
    fn dump_windowed_matches_full() {
        // collect_events_bounded falls back to the full batched path when
        // windowed is off, so `full` here is the ground truth.
        let windows: [(i64, Option<i64>); 6] = [
            (0, Some(100)),
            (0, Some(10)),
            (5, Some(15)),
            (12, Some(20)),
            (18, None),
            (21, Some(24)),
        ];
        for (t0, t1) in windows {
            let mut full = mk(false);
            let mut win = mk(true);
            let (fe, ft, ftr) = full.collect_events_bounded(t0, t1, None, 0, 2);
            let (we, wt, wtr) = win.collect_events_bounded(t0, t1, None, 0, 2);
            let fv: Vec<(i64, Sid, RawValue)> =
                fe.iter().map(|e| (e.tick, e.sid, e.value.clone())).collect();
            let wv: Vec<(i64, Sid, RawValue)> =
                we.iter().map(|e| (e.tick, e.sid, e.value.clone())).collect();
            assert_eq!(fv, wv, "dump events mismatch for [{t0}, {t1:?}]");
            assert_eq!(ft, wt, "dump total mismatch for [{t0}, {t1:?}]");
            assert_eq!(ftr, wtr, "dump truncated mismatch for [{t0}, {t1:?}]");
        }
    }

    // The three consumer equivalences again, against the WLF seed shape:
    // carried value at `from - 1` instead of its true tick. Values,
    // definedness, and dump's event sets must be indistinguishable from
    // true-tick seeds.

    #[test]
    fn snapshot_wlf_seed_shape_matches_full() {
        let mut full = mk(false);
        full.ensure_all_loaded();
        let mut win = mk_wlf_shape();
        for t in [-1, 0, 2, 3, 5, 6, 8, 12, 15, 17, 18, 20, 25, 100] {
            let a = full.snapshot(t, None);
            let b = win.snapshot_streaming(t, None, 2);
            assert_eq!(a, b, "snapshot mismatch at t={t}");
        }
    }

    #[test]
    fn compare_pair_wlf_seed_shape_matches_full() {
        let mut full = mk(false);
        full.ensure_all_loaded();
        let mut win = mk_wlf_shape();
        for (ta, tb) in [(-1, 0), (0, 12), (3, 15), (5, 20), (8, 25), (0, 100)] {
            let (fa, fb) = full.snapshot_pair(ta, tb, None);
            let (wa, wb) = win.snapshot_pair_streaming(ta, tb, None, 2);
            assert_eq!(fa, wa, "compare-a mismatch at ta={ta}");
            assert_eq!(fb, wb, "compare-b mismatch at tb={tb}");
        }
    }

    #[test]
    fn dump_wlf_seed_shape_matches_full() {
        let windows: [(i64, Option<i64>); 6] = [
            (0, Some(100)),
            (0, Some(10)),
            (5, Some(15)),
            (12, Some(20)),
            (18, None),
            (21, Some(24)),
        ];
        for (t0, t1) in windows {
            let mut full = mk(false);
            let mut win = mk_wlf_shape();
            let (fe, ft, ftr) = full.collect_events_bounded(t0, t1, None, 0, 2);
            let (we, wt, wtr) = win.collect_events_bounded(t0, t1, None, 0, 2);
            let fv: Vec<(i64, Sid, RawValue)> =
                fe.iter().map(|e| (e.tick, e.sid, e.value.clone())).collect();
            let wv: Vec<(i64, Sid, RawValue)> =
                we.iter().map(|e| (e.tick, e.sid, e.value.clone())).collect();
            assert_eq!(fv, wv, "dump events mismatch for [{t0}, {t1:?}]");
            assert_eq!(ft, wt, "dump total mismatch for [{t0}, {t1:?}]");
            assert_eq!(ftr, wtr, "dump truncated mismatch for [{t0}, {t1:?}]");
        }
    }
}
