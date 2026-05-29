// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! [`WaveformBackend`] implementation backed by the `wellen` crate.
//!
//! This is the only module in the crate that depends on `wellen`. It translates
//! wellen's hierarchy/signal API into the format-neutral types in the parent
//! module. Swapping in a different parser (or adding a native reader for a new
//! format) means writing a sibling of this file; nothing else changes.

use std::cell::RefCell;

use wellen::simple::Waveform;
use wellen::{
    FileFormat as WFileFormat, Hierarchy, Signal, SignalRef, TimescaleUnit, Var, VarType,
};

use super::{
    BackendError, BackendSid, FileFormat, RawValue, SignalTrace, Timescale, VarDecl,
    WaveformBackend,
};
use crate::format::ValueKind;

/// A waveform loaded through wellen.
pub struct WellenBackend {
    wave: Waveform,
    path: String,
    /// SignalRef indices already materialized inside `wave` (so repeated
    /// `load_traces` calls don't reload). wellen owns the loaded `Signal`s; we
    /// borrow them when decoding.
    loaded: RefCell<std::collections::BTreeSet<usize>>,
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
        })
    }

    fn hierarchy(&self) -> &Hierarchy {
        self.wave.hierarchy()
    }

    /// Absolute tick for a wellen time-table index.
    #[inline]
    fn tick_at(&self, idx: u32) -> i64 {
        self.wave.time_table()[idx as usize] as i64
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
            // conversion basis (matching the reference default) while leaving
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
        let mut out = Vec::new();
        for var in h.all_vars() {
            let (type_str, kind) = vartype_to_str_kind(var.var_type());
            out.push(VarDecl {
                full_path: display_full_name(var, h),
                scope_path: parent_scope_path(var, h),
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
}

impl WellenBackend {
    /// Decode one wellen signal into a [`SignalTrace`]. Walks the signal's
    /// change list sequentially via `iter_changes`, which is the cheapest way
    /// to materialize the whole history (one pass, no per-change binary
    /// search). Time indices are resolved to absolute ticks here.
    fn decode_signal(&self, sig: &Signal) -> SignalTrace {
        let times_idx = sig.time_indices();
        let n = times_idx.len();
        let mut times = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);

        // iter_changes yields (TimeTableIdx, SignalValueRef) in order.
        for (tidx, val) in sig.iter_changes() {
            times.push(self.tick_at(tidx));
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
        R::BitVec(_) => RawValue::Bits(val.to_bit_string().unwrap_or_default()),
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

/// Display path, folding a multi-bit `[msb:lsb]` range into the name to match
/// conventional VCD display (`tb.data[7:0]`). Scalars and 1-bit selects keep
/// the plain name; wellen already reassembles bit-exploded buses.
fn display_full_name(var: &Var, h: &Hierarchy) -> String {
    let base = var.full_name(h);
    if let Some(idx) = var.index() {
        if var.length(h).unwrap_or(0) > 1 {
            return format!("{base}[{}:{}]", idx.msb(), idx.lsb());
        }
    }
    base
}

/// Parent scope path via wellen's scope metadata. Because wellen builds the
/// full name as `parent.local`, and we know the exact local name, we strip the
/// `.local` suffix precisely rather than splitting on '.', so escaped
/// identifiers containing dots stay correct. Returns "" for top-level vars.
fn parent_scope_path(var: &Var, h: &Hierarchy) -> String {
    let full = var.full_name(h);
    let local = var.name(h);
    if full.len() > local.len() + 1 && full.ends_with(local) {
        let cut = full.len() - local.len() - 1;
        if full.as_bytes().get(cut) == Some(&b'.') {
            return full[..cut].to_string();
        }
    }
    String::new()
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
