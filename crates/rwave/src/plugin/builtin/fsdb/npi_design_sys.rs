// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Bindings for Verdi's NPI L1 connectivity layer (`libnpiL1.so`).
//!
//! Loaded lazily and separately from `libNPI.so`: the waveform path must keep
//! working on an install where L1 is absent, so a failure here degrades `trace`
//! to a clear error and leaves everything else alone.
//!
//! **Why the `_dump` variants.** L1's richer entry points
//! (`npi_trace_driver_by_hdl2`) take `std::vector&` and `std::string&`
//! arguments, which cannot be called across a C FFI boundary from Rust at all.
//! Their `_dump` siblings take a `FILE*` instead and are otherwise identical —
//! `int npi_trace_driver_dump2(const char*, FILE*, bool, std::vector<void*>*,
//! const trcOption_t&)`, where the vector argument is a *pointer* we pass NULL
//! for and `trcOption_t` is a plain 5-`bool` POD passed by reference. That
//! makes the whole signature C-compatible, so we get L1's full traversal
//! (hierarchy crossing, pass-through, control dependencies) without a shim and
//! without vendoring anything from Synopsys.
//!
//! The text those functions emit is parsed in `design.rs`.

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::Library;

use super::fsdb_sys;
use crate::plugin::builtin::diag::bridge_err;

const ERR_PREFIX: &str = "rwave-fsdb";
const L1_FILENAME: &str = "libnpiL1.so";

/// `trcOption_t` from `npi_L1_type.h`: five C `bool`s, passed by const
/// reference (i.e. by pointer). Verdi's own `trcOptionDefault` is
/// `{true, false, true, true, false}`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrcOption {
    /// Report only edge-triggered event-control signals.
    pub edge_check: bool,
    /// Treat a variable index as a driver.
    pub trc_var_idx: bool,
    /// Report data signals.
    pub report_data: bool,
    /// Report control signals (clock edges, enclosing if/case). Turning this
    /// off is how clock/reset noise is suppressed — at the source, rather than
    /// by pattern-matching signal names downstream.
    pub report_control: bool,
    /// Report statement properties.
    pub trc_stmt_prop: bool,
}

impl TrcOption {
    pub fn new(report_control: bool) -> TrcOption {
        TrcOption {
            edge_check: true,
            trc_var_idx: false,
            report_data: true,
            report_control,
            trc_stmt_prop: false,
        }
    }
}

pub struct LibNpiL1 {
    _library: Library,
    /// `npi_trace_driver_dump2(sig, FILE*, isPassThrough, boundaryVec, opts)`.
    /// Returns the number of result groups written.
    pub trace_driver_dump: unsafe extern "C" fn(
        *const c_char,
        *mut c_void,
        bool,
        *mut c_void,
        *const TrcOption,
    ) -> c_int,
    /// `npi_trace_load_dump2`, same shape.
    pub trace_load_dump: unsafe extern "C" fn(
        *const c_char,
        *mut c_void,
        bool,
        *mut c_void,
        *const TrcOption,
    ) -> c_int,
}

// SAFETY: same contract as LibNpi — immutable after init, and NPI is
// single-threaded per session by the plugin contract.
unsafe impl Send for LibNpiL1 {}
unsafe impl Sync for LibNpiL1 {}

static LIB_L1: OnceLock<Result<LibNpiL1, String>> = OnceLock::new();

/// Load `libnpiL1.so` once. `Err` means connectivity queries are unavailable on
/// this install; the waveform path is unaffected either way.
pub fn ensure_loaded() -> Result<&'static LibNpiL1, String> {
    match LIB_L1.get_or_init(load_once) {
        Ok(l) => Ok(l),
        Err(e) => Err(e.clone()),
    }
}

fn load_once() -> Result<LibNpiL1, String> {
    // libNPI must be in first: L1 is built against it, and its npi_init has to
    // have run before any L1 call. This also means librt is already preloaded.
    fsdb_sys::ensure_loaded()?;

    // libnpiL1.so declares no DT_NEEDED on libNPI.so (confirmed with readelf)
    // yet calls into it, so it can only link if libNPI's symbols are in the
    // global scope. The waveform path loads libNPI with RTLD_LOCAL, which is
    // correct for its own use and must stay that way; re-opening it here with
    // RTLD_GLOBAL promotes the already-loaded object into the global scope
    // (dlopen is refcounted and returns the same handle) without disturbing
    // anything. Without it L1 has nothing to bind its `npi_*` calls to, and
    // under RTLD_LAZY that surfaces at the first call rather than at load.
    if let Some(npi_path) = fsdb_sys::loaded_path() {
        let promoted = unsafe {
            libloading::os::unix::Library::open(
                Some(&npi_path),
                libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
            )
        };
        if let Ok(lib) = promoted {
            // Held rather than dropped for tidiness; the promotion itself is
            // permanent, since l_global is only cleared when an object is
            // unloaded and fsdb_sys' own handle keeps the count above zero.
            std::mem::forget(lib);
        }
    }

    let path = locate_l1();
    let lib = unsafe { Library::new(&path) }.map_err(|e| {
        bridge_err(
            ERR_PREFIX,
            format!(
                "cannot load {}: {e}. Set RWAVE_NPI_L1_LIB to Verdi's libnpiL1.so.",
                path.display()
            ),
        )
    })?;

    macro_rules! sym {
        ($mangled:expr, $sig:ty) => {{
            let s: libloading::Symbol<$sig> = unsafe { lib.get($mangled) }.map_err(|e| {
                bridge_err(
                    ERR_PREFIX,
                    format!(
                        "missing NPI L1 symbol {}: {e}",
                        String::from_utf8_lossy($mangled)
                    ),
                )
            })?;
            *s
        }};
    }

    type DumpFn =
        unsafe extern "C" fn(*const c_char, *mut c_void, bool, *mut c_void, *const TrcOption) -> c_int;

    let trace_driver_dump = sym!(
        b"_Z22npi_trace_driver_dump2PKcP8_IO_FILEbPSt6vectorIPvSaIS4_EERK11trcOption_s\0",
        DumpFn
    );
    let trace_load_dump = sym!(
        b"_Z20npi_trace_load_dump2PKcP8_IO_FILEbPSt6vectorIPvSaIS4_EERK11trcOption_s\0",
        DumpFn
    );

    Ok(LibNpiL1 { _library: lib, trace_driver_dump, trace_load_dump })
}

/// `$RWAVE_NPI_L1_LIB`, else a sibling of the resolved `libNPI.so`, else the
/// bare name for the dynamic loader to resolve.
fn locate_l1() -> PathBuf {
    if let Some(p) = std::env::var_os("RWAVE_NPI_L1_LIB") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("RWAVE_FSDB_LIB") {
        if let Some(dir) = PathBuf::from(p).parent() {
            let sibling = dir.join(L1_FILENAME);
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    if let Some(dir) = super::super::self_path::self_dir() {
        let sibling = dir.join(L1_FILENAME);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(L1_FILENAME)
}

/// Run `f` with a `FILE*` that writes into memory, and return what it wrote.
///
/// L1's dump entry points report through C stdio, so this is the capture. The
/// buffer is fixed-size: a driver list is small (Verdi's own default trace
/// options bound it), and a truncated tail is preferable to an unbounded
/// allocation driven by library output.
pub fn capture_dump<F>(f: F) -> Result<String, String>
where
    F: FnOnce(*mut c_void) -> c_int,
{
    unsafe extern "C" {
        fn fmemopen(buf: *mut c_void, size: usize, mode: *const c_char) -> *mut c_void;
        fn fclose(stream: *mut c_void) -> c_int;
        fn fflush(stream: *mut c_void) -> c_int;
    }

    /// Closes the stream even if `f` unwinds. Without this, an unwind would
    /// free `buf` while glibc still held an open stream pointing into it, and
    /// the flush at process exit would write into freed memory.
    struct FileGuard(*mut c_void);
    impl Drop for FileGuard {
        fn drop(&mut self) {
            unsafe {
                fflush(self.0);
                fclose(self.0);
            }
        }
    }

    const CAP: usize = 1 << 20;
    let mut buf = vec![0u8; CAP];
    let mode = CString::new("w").expect("static");
    // Hand stdio CAP-1 so the final byte stays the NUL from the zeroed
    // allocation and the scan below always terminates in bounds.
    let file = unsafe { fmemopen(buf.as_mut_ptr() as *mut c_void, CAP - 1, mode.as_ptr()) };
    if file.is_null() {
        return Err(bridge_err(
            ERR_PREFIX,
            "could not allocate a capture buffer",
        ));
    }
    let rc = {
        let _guard = FileGuard(file);
        f(file)
    };
    // The count is the number of groups written; negative is an NPI-side
    // failure. Letting that through would surface as "no driver found", which
    // is a different fact.
    if rc < 0 {
        return Err(bridge_err(ERR_PREFIX, format!("NPI trace failed (rc={rc})")));
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    // A full buffer means the library had more to say. Reporting a truncated
    // dump as complete would silently drop drivers and parse the severed last
    // record into a bogus one.
    //
    // The threshold is CAP-2, not CAP-1, because glibc reserves the final byte
    // of the region it was given for a terminator: on overflow it writes
    // `buffer[size-1] = 0`, which with size = CAP-1 lands at CAP-2. A CAP-1
    // threshold is unreachable there. Verified against glibc 2.31.
    if end >= CAP - 2 {
        return Err(bridge_err(
            ERR_PREFIX,
            "NPI trace output exceeded the capture buffer",
        ));
    }
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn fwrite(ptr: *const c_void, size: usize, n: usize, stream: *mut c_void) -> usize;
    }

    /// Write `n` bytes of 'A' into the capture stream.
    fn fill(n: usize) -> Result<String, String> {
        capture_dump(|file| {
            let chunk = vec![b'A'; n];
            unsafe { fwrite(chunk.as_ptr() as *const c_void, 1, n, file) };
            0
        })
    }

    #[test]
    fn a_short_dump_comes_back_whole() {
        assert_eq!(fill(1024).unwrap().len(), 1024);
    }

    #[test]
    fn an_overlong_dump_is_refused_rather_than_silently_cut() {
        // The detection threshold depends on where libc puts its terminator:
        // glibc reserves the last byte of the region it was given, so a
        // CAP-1 threshold never fires and a truncated dump would be reported
        // as a complete driver list.
        let err = fill((1 << 20) + 4096).unwrap_err();
        assert!(err.contains("exceeded the capture buffer"), "got {err}");
    }

    #[test]
    fn a_negative_return_code_is_an_error_not_an_empty_result() {
        let err = capture_dump(|_| -1).unwrap_err();
        assert!(err.contains("NPI trace failed"), "got {err}");
    }
}
