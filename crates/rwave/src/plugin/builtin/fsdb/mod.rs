// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Built-in Synopsys FSDB backend, via Verdi NPI (`npi_fsdb_*`).
//!
//! Copied near-verbatim from a standalone FSDB bridge plugin:
//! [`backend`] and [`fsdb_sys`] are the plugin's source unchanged
//! but for their `use` paths. This module replaces the plugin's `lib.rs` ABI
//! boundary — the trampolines and static vtable below use rwave's in-crate
//! [`crate::plugin::ffi`] types directly, and the vtable is reached through
//! [`vtable`] (a direct call) rather than a `dlopen`ed `rwave_backend`
//! export.
//!
//! `libNPI` itself is `dlopen`ed at runtime — via `$RWAVE_FSDB_LIB` (see
//! [`fsdb_sys`]) — and `npi_init` needs a Verdi install + a Verdi-Ultra
//! license at runtime; nothing Synopsys is linked or shipped in. An external
//! FSDB plugin set via `$RWAVE_PLUGIN_FSDB` overrides this built-in.

#![allow(clippy::missing_safety_doc)] // SAFETY notes are inline at each call

mod backend;
mod diag;
mod fsdb_sys;

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use backend::FsdbBackend;

use crate::plugin::ffi::{
    file_format, RwaveBackend, RwaveEmit, RwaveSession, RwaveVarDecl, RWAVE_BACKEND_ABI_VERSION,
};

// `name` equals the file extension this backend claims; `version` tracks
// rwave's own version, since the backend now ships inside the binary.
static PLUGIN_NAME: &[u8] = b"fsdb\0";
static PLUGIN_VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

static VTABLE: RwaveBackend = RwaveBackend {
    abi_version: RWAVE_BACKEND_ABI_VERSION,
    name: PLUGIN_NAME.as_ptr() as *const c_char,
    version: PLUGIN_VERSION.as_ptr() as *const c_char,

    open: Some(api_open),
    close: Some(api_close),
    free_err: Some(api_free_err),

    file_format: Some(api_file_format),
    timescale: Some(api_timescale),
    date: Some(api_date),
    version_str: Some(api_version_str),
    time_range: Some(api_time_range),
    time_step_count: Some(api_time_step_count),

    var_decls: Some(api_var_decls),
    load_traces: Some(api_load_traces),
};

/// Resolve the built-in FSDB vtable, loading `libNPI` (and running
/// `npi_init`) on first call. `Err` carries the vendor-library load
/// diagnostic (e.g. `$RWAVE_FSDB_LIB` unset / missing) for the caller to
/// surface verbatim; a missing Verdi license surfaces later at open.
pub fn vtable() -> Result<&'static RwaveBackend, String> {
    fsdb_sys::ensure_loaded()?;
    Ok(&VTABLE)
}

// ---------------------------------------------------------------------------
// Trampolines: decode the opaque handle and delegate to `FsdbBackend`.
// ---------------------------------------------------------------------------

unsafe extern "C" fn api_open(
    path: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut RwaveSession {
    if path.is_null() {
        set_err(err_out, "open: path is NULL");
        return std::ptr::null_mut();
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_err(err_out, "open: path is not valid UTF-8");
            return std::ptr::null_mut();
        }
    };
    match FsdbBackend::open(path_str) {
        Ok(b) => Box::into_raw(Box::new(b)) as *mut RwaveSession,
        Err(e) => {
            set_err(err_out, &e);
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn api_close(handle: *mut RwaveSession) {
    if handle.is_null() {
        return;
    }
    // SAFETY: handle was created by Box::into_raw in api_open; reclaim it via
    // Box::from_raw to run Drop (which calls npi_fsdb_close).
    let _ = unsafe { Box::from_raw(handle as *mut FsdbBackend) };
}

unsafe extern "C" fn api_free_err(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: set_err always uses CString::into_raw, paired here.
    let _ = unsafe { CString::from_raw(s) };
}

unsafe extern "C" fn api_file_format(_handle: *mut RwaveSession) -> u32 {
    // External/non-core formats report UNKNOWN per the ABI contract; rwave
    // identifies the format by the vtable's `name` field instead.
    file_format::UNKNOWN
}

unsafe extern "C" fn api_timescale(
    handle: *mut RwaveSession,
    secs_out: *mut f64,
    display_out: *mut *const c_char,
) {
    let b = unsafe { &*(handle as *mut FsdbBackend) };
    let (secs, display) = b.timescale();
    if !secs_out.is_null() {
        unsafe { *secs_out = secs };
    }
    if !display_out.is_null() {
        unsafe { *display_out = display.as_ptr() };
    }
}

unsafe extern "C" fn api_date(handle: *mut RwaveSession) -> *const c_char {
    let b = unsafe { &*(handle as *mut FsdbBackend) };
    b.date_cstr().as_ptr()
}

unsafe extern "C" fn api_version_str(handle: *mut RwaveSession) -> *const c_char {
    let b = unsafe { &*(handle as *mut FsdbBackend) };
    b.version_cstr().as_ptr()
}

unsafe extern "C" fn api_time_range(
    handle: *mut RwaveSession,
    lo_out: *mut i64,
    hi_out: *mut i64,
) -> c_int {
    let b = unsafe { &*(handle as *mut FsdbBackend) };
    match b.time_range() {
        Some((lo, hi)) => {
            if !lo_out.is_null() {
                unsafe { *lo_out = lo };
            }
            if !hi_out.is_null() {
                unsafe { *hi_out = hi };
            }
            1
        }
        None => 0,
    }
}

unsafe extern "C" fn api_time_step_count(handle: *mut RwaveSession) -> usize {
    let b = unsafe { &*(handle as *mut FsdbBackend) };
    b.time_step_count()
}

unsafe extern "C" fn api_var_decls(
    handle: *mut RwaveSession,
    buf: *mut RwaveVarDecl,
    cap: usize,
) -> usize {
    let b = unsafe { &mut *(handle as *mut FsdbBackend) };
    unsafe { b.var_decls(buf, cap) }
}

unsafe extern "C" fn api_load_traces(
    handle: *mut RwaveSession,
    sids: *const u64,
    n_sids: usize,
    emit: RwaveEmit,
    ctx: *mut c_void,
) -> c_int {
    let b = unsafe { &mut *(handle as *mut FsdbBackend) };
    b.load_traces(sids, n_sids, emit, ctx)
}

/// Paired with `api_free_err` via `CString::into_raw` / `from_raw`.
fn set_err(err_out: *mut *mut c_char, msg: &str) {
    if err_out.is_null() {
        return;
    }
    let Ok(cstr) = CString::new(msg) else {
        return;
    };
    unsafe { *err_out = cstr.into_raw() };
}
