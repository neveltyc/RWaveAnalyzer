// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Backend abstraction layer.
//!
//! `rwave` separates *what* a waveform contains (the domain model in
//! [`crate::model`]) from *how* it is parsed (a backend). This module defines
//! the format-neutral contract every parser front-end must satisfy. The
//! default backend ([`wellen_backend`]) is built on the `wellen` crate and
//! understands VCD and FST, but nothing above this layer depends on wellen:
//! adding a new format (or a faster/native reader for an existing one) means
//! adding another [`WaveformBackend`] implementation, not touching the command
//! set, formatting, filtering, or condition logic.
//!
//! ## Design for performance
//!
//! The hot path in this tool is replaying value changes in time order. To keep
//! that path monomorphic and free of per-value virtual dispatch, the backend
//! does not expose a "get value at index N" virtual call. Instead, when the
//! caller loads a set of signals, the backend decodes each signal's changes
//! **once** into an owned [`SignalTrace`] (parallel `times` / `values`
//! vectors). All replay, merging, and snapshotting then runs over plain owned
//! slices in the domain layer — no trait calls, no re-decoding, no per-step
//! binary search. The trait surface is therefore small and coarse-grained,
//! which is exactly what keeps dynamic dispatch off the inner loop.

use crate::format::ValueKind;

pub mod bitstr;
pub mod wellen_backend;

pub use bitstr::BitStr;

/// Detected (or declared) container format of a waveform file. Kept neutral so
/// the rest of the program never imports a backend-specific format enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Vcd,
    Fst,
    Ghw,
    Unknown,
}

impl FileFormat {
    /// Short lowercase tag, e.g. for diagnostics.
    pub fn tag(self) -> &'static str {
        match self {
            FileFormat::Vcd => "vcd",
            FileFormat::Fst => "fst",
            FileFormat::Ghw => "ghw",
            FileFormat::Unknown => "unknown",
        }
    }
}

/// A timescale expressed as `factor × 10^exponent` seconds per tick, plus a
/// human display string (e.g. `1ns`, `10ps`). `seconds_per_tick` is the
/// precomputed convenience value used throughout time formatting.
#[derive(Debug, Clone)]
pub struct Timescale {
    pub seconds_per_tick: f64,
    pub display: String,
}

/// Errors a backend can raise while opening or reading a file.
#[derive(Debug)]
pub enum BackendError {
    /// File missing, unreadable, a directory, etc. The message is already
    /// user-facing (no `Error:` prefix).
    Open(String),
    /// The file was found but could not be parsed/decoded.
    Parse(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Open(m) | BackendError::Parse(m) => write!(f, "{m}"),
        }
    }
}

/// Backend-side variable declaration metadata, yielded in declaration order
/// (the order variables appear in the underlying file). The domain layer turns
/// this stream into its own sorted, alias-merged signal table; the backend is
/// not responsible for sorting or merging.
pub struct VarDecl {
    /// Full hierarchical display path, dot-separated, with any multi-bit range
    /// already folded in (e.g. `tb.data[7:0]`). The backend owns the policy for
    /// how a path is rendered, because that is format-specific.
    pub full_path: String,
    /// Parent scope path (everything identifying the enclosing scope), computed
    /// from scope metadata — never by string-splitting the full path, so
    /// escaped identifiers containing dots stay correct.
    pub scope_path: String,
    /// Bit width (1 for scalars; declared vector width; 1 for real/string,
    /// where `kind` disambiguates).
    pub width: u32,
    /// Canonical type string (`wire`, `reg`, `real`, `event`, ...).
    pub type_str: &'static str,
    /// Formatting class for values of this variable.
    pub kind: ValueKind,
    /// Opaque backend handle identifying the underlying signal this variable
    /// maps to. Multiple variables may share one handle (aliases). The domain
    /// layer treats this as an opaque key for grouping and for requesting
    /// traces; it never interprets the value.
    pub backend_sid: BackendSid,
}

/// Opaque, backend-defined identifier for an underlying signal. Two `VarDecl`s
/// with the same `BackendSid` alias the same signal. The domain layer compares
/// and hashes these but never constructs or interprets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BackendSid(pub usize);

/// One signal's fully decoded change history, in time order. `times[i]` is the
/// absolute tick at which the signal took value `values[i]`. The two vectors
/// always have equal length. This is the unit of data the merge/snapshot code
/// consumes; producing it is the backend's one performance-critical job.
pub struct SignalTrace {
    pub times: Vec<i64>,
    pub values: Vec<RawValue>,
}

impl SignalTrace {
    pub fn len(&self) -> usize {
        self.times.len()
    }
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }
}

/// A decoded value, owned and backend-neutral. Logic vectors are materialized
/// as an MSB-first bit string (the canonical comparison/formatting form);
/// reals and strings carry their literal payload; events carry no payload.
///
/// Owning the data here (rather than borrowing from the backend) is what lets
/// the entire replay path run without holding a backend borrow, and is also
/// what makes traces cacheable and cheaply comparable. Logic vectors use
/// [`BitStr`], which keeps short values (the overwhelming majority) inline and
/// off the heap.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    Bits(BitStr),
    Real(f64),
    Str(String),
    Event,
}

/// The contract a parser front-end implements. Construction (opening a file) is
/// backend-specific and lives on the concrete type; this trait covers
/// everything the domain layer needs afterwards.
///
/// Method granularity is deliberately coarse: metadata accessors are cheap and
/// called rarely, while bulk value access goes through [`load_traces`] exactly
/// once per signal. There is intentionally no per-sample virtual method.
pub trait WaveformBackend {
    /// File path as opened (for `info` output and diagnostics).
    fn path(&self) -> &str;

    /// Detected container format.
    fn file_format(&self) -> FileFormat;

    /// Timescale (seconds-per-tick + display string).
    fn timescale(&self) -> Timescale;

    /// `$date`-style metadata, or empty if absent.
    fn date(&self) -> &str;

    /// Writer/version metadata, or empty if absent.
    fn version(&self) -> &str;

    /// Free-form comment lines preserved from the file (may be empty if the
    /// backend does not retain them).
    fn comments(&self) -> Vec<String>;

    /// Iterate variable declarations in declaration order.
    fn var_decls(&self) -> Vec<VarDecl>;

    /// Inclusive min/max tick across the whole file, or `None` if there are no
    /// recorded time steps.
    fn time_range(&self) -> Option<(i64, i64)>;

    /// Total number of recorded time steps (distinct timestamps).
    fn time_step_count(&self) -> usize;

    /// Decode the change histories of the given signals, in the same order as
    /// `sids`. Implementations should load lazily and may cache internally;
    /// callers invoke this once per signal set and reuse the result. Unknown
    /// handles yield an empty trace.
    fn load_traces(&mut self, sids: &[BackendSid]) -> Vec<SignalTrace>;
}
