// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! [`WaveformBackend`] implementation backed by the `wellen` crate.
//!
//! This is the only module in the crate that depends on `wellen`. It translates
//! wellen's hierarchy/signal API into the format-neutral types in the parent
//! module. Swapping in a different parser (or adding a native reader for a new
//! format) means writing a sibling of this file; nothing else changes.
//! (The windowed FST fast path lives in the sibling [`super::fst_window`],
//! which depends on `fst-reader` — the same vendored parser wellen reads
//! FST with.)

use std::cell::RefCell;

use wellen::simple::Waveform;
use wellen::{
    FileFormat as WFileFormat, Hierarchy, Signal, SignalEncoding, SignalRef, TimescaleUnit, Var,
    VarType,
};

use super::fst_window::{FstWindowReader, WinKind};
use super::{
    BackendError, BackendSid, BitStr, FileFormat, RawValue, SignalTrace, Timescale, VarDecl,
    WaveformBackend,
};
use crate::format::ValueKind;

/// Lazily created windowed FST reader (see [`WaveformBackend::load_traces_windowed`]).
enum WindowState {
    Unopened,
    Ready(FstWindowReader),
    /// Re-opening the file for windowed access failed (e.g. an incomplete
    /// dump wellen recovered through its own fallback); stay on full decodes.
    Unavailable,
}

/// A waveform loaded through wellen.
pub struct WellenBackend {
    wave: Waveform,
    path: String,
    /// SignalRef indices already materialized inside `wave` (so repeated
    /// `load_traces` calls don't reload). wellen owns the loaded `Signal`s; we
    /// borrow them when decoding.
    loaded: RefCell<std::collections::BTreeSet<usize>>,
    /// Second reader over the same file serving windowed (seek-by-time)
    /// queries on FST; other formats never touch it.
    window: WindowState,
}

impl WellenBackend {
    /// Open a file, auto-detecting the format. Distinguishes "cannot open"
    /// (missing/unreadable/dir) from "parse failed" so the CLI can choose the
    /// right message.
    pub fn open(path: &str) -> Result<WellenBackend, BackendError> {
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BackendError::Open(format!(
                    "cannot open waveform file: {path}"
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(BackendError::Open(format!("permission denied: {path}")));
            }
            Ok(m) if m.is_dir() => {
                return Err(BackendError::Open(format!("not a file: {path}")));
            }
            _ => {}
        }

        let wave = wellen::simple::read(path)
            .map_err(|e| BackendError::Parse(format!("failed to read waveform: {e}")))?;

        Ok(WellenBackend {
            wave,
            path: path.to_string(),
            loaded: RefCell::new(std::collections::BTreeSet::new()),
            window: WindowState::Unopened,
        })
    }

    fn hierarchy(&self) -> &Hierarchy {
        self.wave.hierarchy()
    }
}

impl WaveformBackend for WellenBackend {
    fn path(&self) -> &str {
        &self.path
    }

    fn file_format(&self) -> FileFormat {
        match self.hierarchy().file_format() {
            WFileFormat::Vcd => FileFormat::Vcd,
            WFileFormat::Fst => FileFormat::Fst,
            WFileFormat::Ghw => FileFormat::Ghw,
            _ => FileFormat::Unknown,
        }
    }

    fn timescale(&self) -> Timescale {
        match self.hierarchy().timescale() {
            Some(ts) => {
                let factor = ts.factor.max(1) as f64;
                let seconds_per_tick = match ts.unit.to_exponent() {
                    Some(e) => factor * 10f64.powi(e as i32),
                    None => factor,
                };
                let unit = unit_str(ts.unit);
                let display = if ts.factor <= 1 {
                    format!("1{unit}")
                } else {
                    format!("{}{}", ts.factor, unit)
                };
                Timescale {
                    seconds_per_tick,
                    display,
                }
            }
            // No timescale declared: ticks are unitless. Use 1ps as the
            // conversion basis (a sensible default for unitless dumps) while leaving
            // the display string empty.
            None => Timescale {
                seconds_per_tick: 1e-12,
                display: String::new(),
            },
        }
    }

    fn date(&self) -> &str {
        self.hierarchy().date()
    }

    fn version(&self) -> &str {
        self.hierarchy().version()
    }

    fn comments(&self) -> Vec<String> {
        // wellen's simple API does not preserve VCD $comment blocks.
        Vec::new()
    }

    fn var_decls(&self) -> Vec<VarDecl> {
        let h = self.hierarchy();
        let vars = h.all_vars();
        let (lo, _) = vars.size_hint();
        let mut out = Vec::with_capacity(lo);
        for var in h.all_vars() {
            let (type_str, kind) = vartype_to_str_kind(var.var_type());
            // `full_name` walks the scope tree and allocates; compute it ONCE and
            // derive both the display path and the parent-scope path from it,
            // rather than calling it twice (it dominated table-build time).
            let (full_path, scope_path) = display_and_scope(var, h);
            out.push(VarDecl {
                full_path,
                scope_path,
                width: var_width(var, h),
                type_str,
                kind,
                backend_sid: BackendSid(var.signal_ref().index()),
            });
        }
        out
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        let tt = self.wave.time_table();
        if tt.is_empty() {
            None
        } else {
            Some((tt[0] as i64, tt[tt.len() - 1] as i64))
        }
    }

    fn time_step_count(&self) -> usize {
        self.wave.time_table().len()
    }

    fn load_traces(&mut self, sids: &[BackendSid]) -> Vec<SignalTrace> {
        // Phase 1: ensure every requested signal is materialized in wellen.
        let mut to_load: Vec<SignalRef> = Vec::new();
        {
            let loaded = self.loaded.borrow();
            for s in sids {
                if !loaded.contains(&s.0) {
                    if let Some(r) = SignalRef::from_index(s.0) {
                        to_load.push(r);
                    }
                }
            }
        }
        if !to_load.is_empty() {
            to_load.sort_by_key(|r| r.index());
            to_load.dedup_by_key(|r| r.index());
            self.wave.load_signals_multi_threaded(&to_load);
            let mut loaded = self.loaded.borrow_mut();
            for r in &to_load {
                loaded.insert(r.index());
            }
        }

        // Phase 2: decode each requested signal's change history once.
        sids.iter()
            .map(|s| {
                let sref = match SignalRef::from_index(s.0) {
                    Some(r) => r,
                    None => return empty_trace(),
                };
                match self.wave.get_signal(sref) {
                    Some(sig) => self.decode_signal(sig),
                    None => empty_trace(),
                }
            })
            .collect()
    }

    // FST is the one wellen format that can seek: its data sections are
    // time-ranged, so a window touches only the sections around it. VCD has
    // no index (the whole body is parsed at open) and GHW's section positions
    // are not used by wellen, so both stay on the full decode.
    fn supports_windowed(&self) -> bool {
        self.file_format() == FileFormat::Fst
    }

    fn load_traces_windowed(
        &mut self,
        sids: &[BackendSid],
        from: i64,
        to: Option<i64>,
    ) -> Vec<SignalTrace> {
        if self.file_format() != FileFormat::Fst {
            return self.load_traces(sids);
        }
        // Resolve each sid to an FST handle plus payload kind. Anything the
        // windowed reader cannot serve exactly — derived signals (which FST
        // hierarchies never produce) or unknown handles — routes the whole
        // request to the full decode, which is always a correct answer.
        let mut slots: Vec<(usize, WinKind)> = Vec::with_capacity(sids.len());
        for s in sids {
            let kind = SignalRef::from_index(s.0)
                .filter(|r| !r.is_derived_signal())
                .and_then(|r| self.hierarchy().get_signal_tpe(r))
                .map(|enc| match enc {
                    SignalEncoding::String => WinKind::Str,
                    SignalEncoding::Real => WinKind::Real,
                    SignalEncoding::BitVector(0) => WinKind::Event,
                    SignalEncoding::BitVector(_) => WinKind::Bits,
                });
            match kind {
                Some(k) => slots.push((s.0, k)),
                None => return self.load_traces(sids),
            }
        }

        if matches!(self.window, WindowState::Unopened) {
            self.window = match FstWindowReader::open(&self.path) {
                Some(r) => WindowState::Ready(r),
                None => WindowState::Unavailable,
            };
        }
        let WindowState::Ready(reader) = &mut self.window else {
            return self.load_traces(sids);
        };
        let traces = reader.load_windowed(&slots, from, to, self.wave.time_table());
        if traces.len() != sids.len() {
            // Mid-stream read failure: serve the correct full decode instead.
            return self.load_traces(sids);
        }
        traces
    }
}

impl WellenBackend {
    /// Decode one wellen signal into a [`SignalTrace`]. Walks the signal's
    /// change list sequentially via `iter_changes`, which is the cheapest way
    /// to materialize the whole history (one pass, no per-change binary
    /// search). Time indices are resolved to absolute ticks here.
    fn decode_signal(&self, sig: &Signal) -> SignalTrace {
        let n = sig.time_indices().len();
        let mut times = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);

        // Hoist the time-table slice out of the hot loop so resolving each
        // change's absolute tick is a single indexed load, not a method call
        // per change (this loop runs tens of millions of times on large files).
        let time_table = self.wave.time_table();

        // iter_changes yields (TimeTableIdx, SignalValueRef) in order.
        for (tidx, val) in sig.iter_changes() {
            times.push(time_table[tidx as usize] as i64);
            values.push(decode_value(val));
        }

        // Defensive: if iter_changes and time_indices disagree in length (they
        // shouldn't), trust whichever is shorter to keep the vectors aligned.
        if times.len() != values.len() {
            let m = times.len().min(values.len());
            times.truncate(m);
            values.truncate(m);
        }

        SignalTrace { times, values }
    }
}

fn empty_trace() -> SignalTrace {
    SignalTrace {
        times: Vec::new(),
        values: Vec::new(),
    }
}

/// Decode a borrowed wellen value into an owned, neutral [`RawValue`].
fn decode_value(val: wellen::SignalValueRef<'_>) -> RawValue {
    use wellen::SignalValueRef as R;
    match val {
        R::Event => RawValue::Event,
        R::BitVec(bv) => {
            // Build the MSB-first bit string straight from wellen's bit iterator
            // into a `BitStr`, which keeps short values (the overwhelming
            // majority) inline. This avoids the per-change heap `String` that
            // `to_bit_string()` would allocate. `width()` gives the exact bit
            // count, matching the number of chars the iterator yields.
            let width = bv.width() as usize;
            RawValue::Bits(BitStr::from_ascii_iter(
                width,
                bv.iter_msb_to_lsb().map(|b| b.as_ascii()),
            ))
        }
        R::Real(x) => RawValue::Real(x),
        R::String(s) => RawValue::Str(s.to_string()),
    }
}

/// Map a wellen `VarType` to a canonical type string and a formatting kind.
fn vartype_to_str_kind(vt: VarType) -> (&'static str, ValueKind) {
    use VarType::*;
    match vt {
        Event => ("event", ValueKind::Event),
        Integer => ("integer", ValueKind::Bits),
        Parameter => ("parameter", ValueKind::Bits),
        Real => ("real", ValueKind::Real),
        Reg => ("reg", ValueKind::Bits),
        Supply0 => ("supply0", ValueKind::Bits),
        Supply1 => ("supply1", ValueKind::Bits),
        Time => ("time", ValueKind::Bits),
        Tri => ("tri", ValueKind::Bits),
        TriAnd => ("triand", ValueKind::Bits),
        TriOr => ("trior", ValueKind::Bits),
        TriReg => ("trireg", ValueKind::Bits),
        Tri0 => ("tri0", ValueKind::Bits),
        Tri1 => ("tri1", ValueKind::Bits),
        WAnd => ("wand", ValueKind::Bits),
        Wire => ("wire", ValueKind::Bits),
        WOr => ("wor", ValueKind::Bits),
        String => ("string", ValueKind::Str),
        Port => ("port", ValueKind::Bits),
        SparseArray => ("sparsearray", ValueKind::Bits),
        RealTime => ("realtime", ValueKind::Real),
        RealParameter => ("realparameter", ValueKind::Real),
        Bit => ("bit", ValueKind::Bits),
        Logic => ("logic", ValueKind::Bits),
        Int => ("int", ValueKind::Bits),
        ShortInt => ("shortint", ValueKind::Bits),
        LongInt => ("longint", ValueKind::Bits),
        Byte => ("byte", ValueKind::Bits),
        Enum => ("enum", ValueKind::Bits),
        ShortReal => ("shortreal", ValueKind::Real),
        Boolean => ("boolean", ValueKind::Bits),
        BitVector => ("bit_vector", ValueKind::Bits),
        StdLogic => ("std_logic", ValueKind::Bits),
        StdLogicVector => ("std_logic_vector", ValueKind::Bits),
        StdULogic => ("std_ulogic", ValueKind::Bits),
        StdULogicVector => ("std_ulogic_vector", ValueKind::Bits),
        EventParameter => ("event", ValueKind::Event),
    }
}

/// Bit width of a variable: signal encoding length if known, else the declared
/// `[msb:lsb]` width, else 1.
fn var_width(var: &Var, h: &Hierarchy) -> u32 {
    if let Some(len) = var.length(h) {
        if len > 0 {
            return len;
        }
    }
    if let Some(idx) = var.index() {
        return idx.width();
    }
    1
}

/// Compute a variable's display path and parent-scope path from a single
/// `full_name` call. `full_name` reconstructs the dotted hierarchical path by
/// walking the scope tree and allocating — calling it once per variable instead
/// of twice roughly halves that cost across a large hierarchy.
///
/// * Display path: `full_name`, with a multi-bit `[msb:lsb]` range folded in to
///   match conventional VCD display (`tb.data[7:0]`). Scalars and 1-bit selects
///   keep the plain name; wellen already reassembles bit-exploded buses.
/// * Scope path: `full_name` with its trailing `.<local>` removed, computed by
///   stripping the exact local-name suffix rather than splitting on '.', so
///   escaped identifiers containing dots stay correct. `""` for top-level vars.
fn display_and_scope(var: &Var, h: &Hierarchy) -> (String, String) {
    let full = var.full_name(h);
    let local = var.name(h);

    // Parent scope: strip a trailing ".<local>" if present.
    let scope = {
        if full.len() > local.len() + 1
            && full.ends_with(local)
            && full.as_bytes()[full.len() - local.len() - 1] == b'.'
        {
            full[..full.len() - local.len() - 1].to_string()
        } else {
            String::new()
        }
    };

    // Display name: fold a multi-bit [msb:lsb] range into the name.
    let display = match var.index() {
        Some(idx) if var.length(h).unwrap_or(0) > 1 => {
            format!("{full}[{}:{}]", idx.msb(), idx.lsb())
        }
        _ => full,
    };

    (display, scope)
}

fn unit_str(u: TimescaleUnit) -> &'static str {
    match u {
        TimescaleUnit::ZeptoSeconds => "zs",
        TimescaleUnit::AttoSeconds => "as",
        TimescaleUnit::FemtoSeconds => "fs",
        TimescaleUnit::PicoSeconds => "ps",
        TimescaleUnit::NanoSeconds => "ns",
        TimescaleUnit::MicroSeconds => "us",
        TimescaleUnit::MilliSeconds => "ms",
        TimescaleUnit::Seconds => "s",
        TimescaleUnit::Unknown => "",
    }
}

#[cfg(test)]
mod windowed_fst_tests {
    //! `load_traces_windowed` on real FST files must return exactly the
    //! window-slice of the full decode: the signal's last change at-or-before
    //! `from` (true value *and* true timestamp), then every change in
    //! `(from, to]` — nothing synthesized, nothing missing. These tests sweep
    //! window grids over every bundled FST fixture (single-section files:
    //! phase 1 + empty-phase-2 paths) and, when present, the multi-section
    //! bench trace (real section skipping + phase-2 seed recovery).

    use super::*;

    fn repo_path(rel: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    /// Oracle: slice a full trace to the windowed contract.
    fn window_slice(full: &SignalTrace, from: i64, to: Option<i64>) -> (Vec<i64>, Vec<String>) {
        let mut times = Vec::new();
        let mut values = Vec::new();
        if let Some(i) = full.times.iter().rposition(|&t| t <= from) {
            times.push(full.times[i]);
            values.push(full.values[i].raw_str().into_owned());
        }
        for (i, &t) in full.times.iter().enumerate() {
            if t > from && to.is_none_or(|hi| t <= hi) {
                times.push(t);
                values.push(full.values[i].raw_str().into_owned());
            }
        }
        (times, values)
    }

    fn flat(tr: &SignalTrace) -> (Vec<i64>, Vec<String>) {
        (
            tr.times.clone(),
            tr.values.iter().map(|v| v.raw_str().into_owned()).collect(),
        )
    }

    /// Sweep `windows` over all signals of `path`, comparing windowed decode
    /// against the sliced full decode.
    fn check_file(path: &std::path::Path, windows: &[(i64, Option<i64>)]) {
        let mut b = WellenBackend::open(path.to_str().unwrap()).expect("open fst");
        assert!(b.supports_windowed(), "{path:?} should support windowed");
        let mut bsids: Vec<BackendSid> = b.var_decls().iter().map(|v| v.backend_sid).collect();
        bsids.sort_by_key(|s| s.0);
        bsids.dedup_by_key(|s| s.0);
        let full = b.load_traces(&bsids);
        for &(from, to) in windows {
            let win = b.load_traces_windowed(&bsids, from, to);
            assert_eq!(win.len(), bsids.len());
            for ((sid, f), w) in bsids.iter().zip(&full).zip(&win) {
                assert_eq!(
                    window_slice(f, from, to),
                    flat(w),
                    "{path:?} sid={} window=[{from}, {to:?}]",
                    sid.0
                );
            }
        }
    }

    /// Window grid derived from a file's recorded time steps: every point
    /// window, every ordered pair (strided to stay small), edges and
    /// out-of-range probes, and unbounded tails.
    fn grid(path: &std::path::Path) -> Vec<(i64, Option<i64>)> {
        let b = WellenBackend::open(path.to_str().unwrap()).expect("open fst");
        let (t0, t1) = b.time_range().expect("time range");
        let steps: Vec<i64> = {
            // Reconstruct the recorded steps from the traces (the backend has
            // no public time-table accessor; the union of change times is it).
            let bsids: Vec<BackendSid> = b.var_decls().iter().map(|v| v.backend_sid).collect();
            let mut b2 = b;
            let mut all: Vec<i64> = b2
                .load_traces(&bsids)
                .iter()
                .flat_map(|t| t.times.iter().copied())
                .collect();
            all.sort_unstable();
            all.dedup();
            all
        };
        let mut w: Vec<(i64, Option<i64>)> = Vec::new();
        for &t in &steps {
            w.push((t, Some(t))); // point
            w.push((t - 1, Some(t + 1)));
            w.push((t, None)); // unbounded tail
        }
        let stride = (steps.len() / 12).max(1);
        for (i, &a) in steps.iter().step_by(stride).enumerate() {
            for &bt in steps.iter().skip(i * stride).step_by(stride) {
                if bt >= a {
                    w.push((a, Some(bt)));
                }
            }
        }
        w.push((t0 - 10, None));
        w.push((t0 - 10, Some(t0 - 1))); // entirely before the dump
        w.push((t1 + 10, Some(t1 + 20))); // entirely after the dump
        w.push((t1 + 10, None));
        w.push(((t0 + t1) / 2, None));
        w
    }

    #[test]
    fn fixtures_windowed_equals_full_slice() {
        let files = [
            "verify/fixtures/basic_trace.fst",
            "verify/fixtures/bus_range_trace.fst",
            "verify/fixtures/escaped_trace.fst",
            "verify/fixtures/handshake_trace.fst",
            "verify/fixtures/search_trace.fst",
            "verify/stimulus/counter_fsm.fst",
            "verify/stimulus/handshake_proto.fst",
            "verify/stimulus/hier_deep.fst",
            "verify/stimulus/real_event.fst",
            "verify/stimulus/xz_tristate.fst",
        ];
        for rel in files {
            let path = repo_path(rel);
            let windows = grid(&path);
            check_file(&path, &windows);
        }
    }

    #[test]
    fn vcd_does_not_claim_windowed() {
        let path = repo_path("verify/fixtures/basic_trace.vcd");
        let b = WellenBackend::open(path.to_str().unwrap()).expect("open vcd");
        assert!(!b.supports_windowed());
    }

    /// Multi-section coverage: 84 data sections, so mid/late windows skip most
    /// of the file and cold signals (last change far before the window) must
    /// come back through phase 2. Skipped when the decompressed bench trace is
    /// absent (`bench/stress.fst` is git-ignored; `bench/run.py` creates it).
    #[test]
    fn stress_multi_section_windowed_equals_full_slice() {
        let path = repo_path("bench/stress.fst");
        if !path.exists() {
            eprintln!("skipping: {path:?} not present (run bench/run.py once to create it)");
            return;
        }
        let mut b = WellenBackend::open(path.to_str().unwrap()).expect("open stress fst");
        let mut bsids: Vec<BackendSid> = b.var_decls().iter().map(|v| v.backend_sid).collect();
        bsids.sort_by_key(|s| s.0);
        bsids.dedup_by_key(|s| s.0);
        // Sample across the id space: constants, buses, and hot clocks all
        // appear; every ~400th signal keeps the full decode tractable.
        let sample: Vec<BackendSid> = bsids.iter().copied().step_by(400).collect();
        let full = b.load_traces(&sample);
        // Ticks span 0..=20_000_000 over 84 sections (~240k ticks each).
        let windows: [(i64, Option<i64>); 7] = [
            (19_900_000, Some(19_950_000)), // late: most sections skipped
            (10_000_000, Some(10_100_000)), // mid
            (0, Some(1_000)),               // head
            (5_000_000, Some(5_000_000)),   // point
            (19_999_990, None),             // unbounded tail
            (20_500_000, Some(21_000_000)), // beyond the dump
            (236_540, Some(236_540)),       // exactly a section boundary tick
        ];
        for (from, to) in windows {
            let win = b.load_traces_windowed(&sample, from, to);
            assert_eq!(win.len(), sample.len());
            for ((sid, f), w) in sample.iter().zip(&full).zip(&win) {
                assert_eq!(
                    window_slice(f, from, to),
                    flat(w),
                    "stress sid={} window=[{from}, {to:?}]",
                    sid.0
                );
            }
        }
    }
}
