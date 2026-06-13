// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Rust mirror of `include/rwave_backend.h`. Backend authors writing in
//! C consume the header directly; backend authors writing in Rust can
//! `pub use` from here to get the same layout types.
//!
//! Every type in this module is `#[repr(C)]` and bit-compatible with the
//! header. The function-pointer fields in [`RwaveBackend`] are wrapped
//! in `Option` so the Rust loader can defensively null-check the vtable
//! (the conformance checklist in `docs/PLUGIN.md` requires non-NULL, but
//! a malformed backend should fail closed rather than UB on first call).

use std::ffi::{c_char, c_int, c_void};

/// Bump only on breaking vtable changes — see [`super`] docs.
pub const RWAVE_BACKEND_ABI_VERSION: u32 = 1;

/// Opaque per-file handle owned by the backend. Sized as a ZST in Rust
/// (`struct` with empty array); we only ever traffic in `*mut RwaveSession`.
#[repr(C)]
pub struct RwaveSession {
    _private: [u8; 0],
}

/// Format-identity values for the built-in formats. Mirrors
/// `RwaveFileFormat` in the C header. Plugin formats report
/// [`Self::Unknown`] — rwave does not maintain per-format constants for
/// them.
///
/// Note: not used as the vtable return type. The vtable's `file_format`
/// field returns a raw `u32` so a misbehaving plugin returning a value
/// outside this enum cannot produce an undefined-discriminant value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // built-in formats are reported by the wellen backend, not via FFI
pub enum RwaveFileFormat {
    Unknown = 0,
    Vcd = 1,
    Fst = 2,
    Ghw = 3,
}

/// Integer values for [`RwaveFileFormat`]. Kept as plain constants so
/// the file_format match in [`crate::backend::plugin_backend`] can
/// dispatch on `u32` without rejecting plugin-returned out-of-range values.
pub mod file_format {
    pub const UNKNOWN: u32 = 0;
    pub const VCD: u32 = 1;
    pub const FST: u32 = 2;
    pub const GHW: u32 = 3;
}

/// Value formatting class for a variable. Mirrors `RwaveValueKind`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RwaveValueKind {
    Bits = 0,
    Real = 1,
    Str = 2,
    Event = 3,
}

/// One variable declaration; mirrors `RwaveVarDecl`. All `*const c_char`
/// pointers are owned by the backend and live as long as the parent
/// `RwaveSession*`.
#[repr(C)]
pub struct RwaveVarDecl {
    pub full_path: *const c_char,
    pub scope_path: *const c_char,
    pub width: u32,
    pub type_str: *const c_char,
    pub kind: RwaveValueKind,
    pub backend_sid: u64,
}

/// Streaming-trace callback signature. The backend calls this once per
/// change event during `load_traces`.
pub type RwaveEmit = unsafe extern "C" fn(
    ctx: *mut c_void,
    backend_sid: u64,
    time_tick: i64,
    value_buf: *const c_char,
    value_len: u32,
);

/// Backend vtable. Mirrors `RwaveBackend` in the C header. Function
/// pointers are `Option<...>` so a NULL slot is observable from Rust
/// without UB; the loader validates non-NULL on every required slot
/// before first use, and rejects mismatched `abi_version`.
#[repr(C)]
pub struct RwaveBackend {
    pub abi_version: u32,
    pub name: *const c_char,
    pub version: *const c_char,

    // lifecycle
    pub open: Option<unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> *mut RwaveSession>,
    pub close: Option<unsafe extern "C" fn(*mut RwaveSession)>,
    pub free_err: Option<unsafe extern "C" fn(*mut c_char)>,

    // metadata
    // file_format returns a raw u32 (ABI-compatible with the C enum
    // RwaveFileFormat) rather than the Rust enum mirror. This way a
    // plugin returning a value outside the enum doesn't produce a Rust
    // invalid-discriminant; the dispatch in plugin_backend maps the
    // u32 to FileFormat::Unknown for unknown values.
    pub file_format: Option<unsafe extern "C" fn(*mut RwaveSession) -> u32>,
    pub timescale:
        Option<unsafe extern "C" fn(*mut RwaveSession, *mut f64, *mut *const c_char)>,
    pub date: Option<unsafe extern "C" fn(*mut RwaveSession) -> *const c_char>,
    pub version_str: Option<unsafe extern "C" fn(*mut RwaveSession) -> *const c_char>,
    pub time_range: Option<unsafe extern "C" fn(*mut RwaveSession, *mut i64, *mut i64) -> c_int>,
    pub time_step_count: Option<unsafe extern "C" fn(*mut RwaveSession) -> usize>,

    // hierarchy
    pub var_decls:
        Option<unsafe extern "C" fn(*mut RwaveSession, *mut RwaveVarDecl, usize) -> usize>,

    // trace decode
    pub load_traces: Option<
        unsafe extern "C" fn(
            *mut RwaveSession,
            *const u64,
            usize,
            RwaveEmit,
            *mut c_void,
        ) -> c_int,
    >,
}

// SAFETY: an `RwaveBackend` is an immutable vtable — function pointers,
// pointers to 'static C strings, and POD. A built-in backend instantiates
// it as a `static`, and rwave only ever reads it; sharing the value across
// threads is sound. (External plugins return a `*const RwaveBackend`, which
// needs no `Sync`, but the bound is harmless to them.)
unsafe impl Sync for RwaveBackend {}

/// Signature of the backend's sole exported symbol. `dlsym`-resolved by
/// the loader as [`RWAVE_BACKEND_SYMBOL`].
pub type RwaveBackendInit =
    unsafe extern "C" fn(err_out: *mut *const c_char) -> *const RwaveBackend;

/// Name of the exported entry-point symbol. Same as the function-pointer
/// type name in the C header (case-sensitive, C linkage).
pub const RWAVE_BACKEND_SYMBOL: &[u8] = b"rwave_backend\0";
