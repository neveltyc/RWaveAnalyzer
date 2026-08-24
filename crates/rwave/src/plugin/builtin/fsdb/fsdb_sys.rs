//! libloading bindings for Synopsys NPI (`libNPI.so`), FSDB reader subset.
//!
//! NPI's `npi_fsdb_*` reader is a C++ API compiled **without** `extern "C"`,
//! so its symbols are Itanium-mangled. They are nonetheless plain *free
//! functions* over opaque `void*` handles + POD structs, so we bind them by
//! their (signature-derived) mangled names — no C++ shim. The manglings are
//! determined by the function signatures and stable across Verdi releases.
//!
//! Resolution order for `libNPI.so`:
//!   1. `$RWAVE_FSDB_LIB` (absolute path to libNPI.so)
//!   2. sibling of this cdylib (located via dladdr — the self-contained bundle)
//!   3. platform loader default (`LD_LIBRARY_PATH`, the sourced Verdi env)
//!
//! `npi_init` is called once per process at load. It checks out a
//! Verdi-Ultra license and locates Verdi's resource dir from the
//! environment; both must be set up by the caller (source the Verdi env, or
//! point at a runtime bundle). Failures surface at `npi_fsdb_open` (NULL).

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;

use libloading::Library;

use super::ERR_PREFIX;
use crate::plugin::builtin::diag::bridge_err;
use crate::plugin::builtin::self_path::self_dir;

/// All NPI FSDB handles (file/scope/sig/sigdb/vct/iter) are opaque `void*`.
pub type NpiHandle = *mut c_void;

/// `npiFsdbTime` is `NPI_UINT64`.
pub type NpiFsdbTime = u64;

/// `npiFsdbValType` selectors (set `NpiFsdbValue::format` before `vct_value`).
pub mod val_fmt {
    pub const BIN_STR: i32 = 0; // MSB-first "01xz" string
    pub const REAL: i32 = 6; // IEEE-754 double
}

/// `npiFsdbSigPropertyType` selectors (subset we read).
pub mod sig_prop {
    pub const FULL_NAME: i32 = 1; // npiFsdbSigFullName  (str)
    pub const IS_REAL: i32 = 2; // npiFsdbSigIsReal    (int 1/0)
    pub const HAS_MEMBER: i32 = 3; // npiFsdbSigHasMember (int 1/0 — composite signal)
    pub const RANGE_SIZE: i32 = 6; // npiFsdbSigRangeSize (int; bit width of a plain vector)
    pub const IS_STRING: i32 = 7; // npiFsdbSigIsString  (int 1/0)
}

/// `npiFsdbFilePropertyType` selectors.
pub mod file_prop {
    pub const SCALE_UNIT: i32 = 1; // npiFsdbFileScaleUnit (str)
    pub const VERSION: i32 = 10; // npiFsdbFileVersion   (str)
    pub const SIM_DATE: i32 = 11; // npiFsdbFileSimDate   (str)
}

/// Mirror of `npiFsdbValue` from `npi_fsdb.h`:
/// `struct { npiFsdbValType format; union {...} value; }`.
/// The union's widest member is 8 bytes (ptr / u64 / f64), so with the
/// 4-byte `format` enum + 4 padding the struct is 16 bytes, align 8 — the
/// layout `#[repr(C)]` reproduces.
#[repr(C)]
pub union NpiFsdbValueUnion {
    pub str_: *const c_char,
    pub sint: i32,
    pub uint: u32,
    pub sint64: i64,
    pub uint64: u64,
    pub real: f64,
}

#[repr(C)]
pub struct NpiFsdbValue {
    pub format: i32,
    pub value: NpiFsdbValueUnion,
}

impl NpiFsdbValue {
    /// A value pre-set to request `format`, union zeroed.
    pub fn with_format(format: i32) -> Self {
        NpiFsdbValue {
            format,
            value: NpiFsdbValueUnion { uint64: 0 },
        }
    }
}


/// Mirror of `npiWaveformInfo` (`npi_fsdb.h`), filled by [`LibNpi::waveform_info`].
///
/// Every field is a pointer or an integer, so the struct is plain C data and
/// can be filled across the FFI boundary directly.
///
/// The trailing `_reserved` is deliberate. The shipped header carries twelve
/// fields, but the *documentation* for the same release still describes the
/// nine-field version ending at `ufeTag` — proof that this struct has grown
/// between releases. The callee writes into storage we own, so a future
/// library that writes more fields would otherwise smash the stack; the slack
/// absorbs it. Nothing reads `_reserved`.
#[repr(C)]
pub struct NpiWaveformInfo {
    pub version: *const c_char,
    pub simulator_type: *const c_char,
    pub scale_unit: *const c_char,
    pub min_time: u64,
    pub max_time: u64,
    pub dump_off_count: u32,
    pub dump_off_range: *const c_char,
    pub has_fsdb_gate: u32,
    pub ufe_tag: *const c_char,
    /// Absolute path of the `simv.daidir` the dump was produced from — the
    /// design library that holds the KDB. Written by VCS into the FSDB header.
    pub simv_daidir_path: *const c_char,
    /// The simulation's working directory.
    pub simv_working_path: *const c_char,
    pub is_completed: u32,
    _reserved: [u64; 8],
}

impl Default for NpiWaveformInfo {
    fn default() -> Self {
        // SAFETY: every field is a pointer or an integer, for which all-zero is
        // a valid (and here, meaningful: "absent") bit pattern.
        unsafe { std::mem::zeroed() }
    }
}

#[allow(dead_code)]
pub struct LibNpi {
    _library: Library,
    /// The file `_library` was opened from. See [`loaded_path`].
    path: PathBuf,

    // lifecycle
    pub fsdb_open: unsafe extern "C" fn(*const c_char) -> NpiHandle,
    pub fsdb_close: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub is_fsdb: unsafe extern "C" fn(*const c_char) -> c_int,

    // file metadata
    pub file_property: unsafe extern "C" fn(c_int, NpiHandle, *mut c_int) -> c_int,
    pub file_property_str: unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char,
    pub min_time: unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int,
    pub max_time: unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int,

    // hierarchy
    pub iter_top_scope: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub iter_child_scope: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub iter_scope_next: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub iter_scope_stop: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub iter_sig: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub iter_sig_next: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub iter_sig_stop: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub scope_property_str: unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char,
    pub sig_property: unsafe extern "C" fn(c_int, NpiHandle, *mut c_int) -> c_int,
    pub sig_property_str: unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char,

    // value-change traverse
    pub create_vct: unsafe extern "C" fn(NpiHandle) -> NpiHandle,
    pub release_vct: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub goto_first: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub goto_next: unsafe extern "C" fn(NpiHandle) -> c_int,
    pub goto_time: unsafe extern "C" fn(NpiHandle, NpiFsdbTime) -> c_int,
    pub vct_time: unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int,
    pub vct_value: unsafe extern "C" fn(NpiHandle, *mut NpiFsdbValue) -> c_int,
}

// SAFETY: Library is Send + Sync; entries are immutable after init. NPI is
// single-threaded per session, which rwave's plugin contract guarantees.
unsafe impl Send for LibNpi {}
unsafe impl Sync for LibNpi {}

/// The design-side NPI entry points, resolved separately from the reader.
///
/// Kept out of [`LibNpi`] on purpose. `fsdb::vtable()` calls
/// [`ensure_loaded`] before any FSDB command opens a file, so a symbol
/// resolved there is a hard requirement for `info`, `list`, `dump` and the
/// rest. These are needed only by `trace`, and an older Verdi that lacks one
/// must still be able to read waveforms: Verdi 2018 has the first three and
/// not `npi_waveform_info`, so binding them eagerly took the whole FSDB
/// backend down on that install.
pub struct LibNpiDesign {
    pub load_design: unsafe extern "C" fn(c_int, *mut *mut c_char) -> c_int,
    /// Resolves a full hierarchical name in the *design*, which is how "not in
    /// the design database" is told apart from "no drivers".
    pub handle_by_name: unsafe extern "C" fn(*const c_char, NpiHandle) -> NpiHandle,
    pub release_handle: unsafe extern "C" fn(NpiHandle) -> c_int,
    /// Header metadata read straight from a file, without opening a session.
    /// `None` on a Verdi predating the call; `trace` then needs `--kdb`
    /// instead of finding the design library on its own.
    pub waveform_info: Option<unsafe extern "C" fn(*const c_char, *mut NpiWaveformInfo) -> c_int>,
}

unsafe impl Send for LibNpiDesign {}
unsafe impl Sync for LibNpiDesign {}

static LIBNPI: OnceLock<Result<LibNpi, String>> = OnceLock::new();
static LIBNPI_DESIGN: OnceLock<Result<LibNpiDesign, String>> = OnceLock::new();

/// Resolve the design-side symbols, once. `Err` means this Verdi cannot answer
/// connectivity queries; the waveform path is unaffected either way.
pub fn design_syms() -> Result<&'static LibNpiDesign, String> {
    ensure_loaded()?;
    match LIBNPI_DESIGN.get_or_init(load_design_syms) {
        Ok(d) => Ok(d),
        Err(e) => Err(e.clone()),
    }
}

fn load_design_syms() -> Result<LibNpiDesign, String> {
    let lib = &npi()._library;
    macro_rules! need {
        ($mangled:expr, $sig:ty) => {{
            let s: libloading::Symbol<$sig> = unsafe { lib.get($mangled) }.map_err(|_| {
                bridge_err(ERR_PREFIX, format!(
                    "this Verdi's libNPI.so has no {}, so it cannot answer design queries; \
                     trace needs a newer Verdi",
                    String::from_utf8_lossy(&$mangled[..$mangled.len() - 1])
                ))
            })?;
            *s
        }};
    }
    let load_design = need!(
        b"_Z15npi_load_designiPPc\0",
        unsafe extern "C" fn(c_int, *mut *mut c_char) -> c_int
    );
    let handle_by_name = need!(
        b"_Z18npi_handle_by_namePKcPv\0",
        unsafe extern "C" fn(*const c_char, NpiHandle) -> NpiHandle
    );
    let release_handle = need!(
        b"_Z18npi_release_handlePv\0",
        unsafe extern "C" fn(NpiHandle) -> c_int
    );
    // Optional: absent before roughly Verdi 2019, and only costs the caller the
    // convenience of not having to pass --kdb.
    let waveform_info = unsafe {
        lib.get::<unsafe extern "C" fn(*const c_char, *mut NpiWaveformInfo) -> c_int>(
            b"_Z17npi_waveform_infoPKcR15npiWaveformInfo\0",
        )
    }
    .ok()
    .map(|s| *s);

    Ok(LibNpiDesign { load_design, handle_by_name, release_handle, waveform_info })
}

pub fn ensure_loaded() -> Result<(), String> {
    match LIBNPI.get_or_init(load_npi_once) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }
}

/// The path `libNPI.so` was actually loaded from.
///
/// Recorded at load rather than re-resolved, so a caller re-opening the same
/// object cannot end up with a *different* file: resolution reads the
/// environment and the executable's directory, and re-running it after a
/// `chdir` or an environment change could map a second, distinct copy of
/// libNPI whose `npi_init` never ran.
pub fn loaded_path() -> Option<PathBuf> {
    LIBNPI.get().and_then(|r| r.as_ref().ok()).map(|l| l.path.clone())
}

pub fn npi() -> &'static LibNpi {
    LIBNPI
        .get()
        .and_then(|r| r.as_ref().ok())
        .expect("libNPI accessed before successful ensure_loaded()")
}

fn load_npi_once() -> Result<LibNpi, String> {
    let path = locate_npi()?;

    // libNPI prints a copyright/version banner from a library constructor at
    // dlopen, and npi_init emits license diagnostics — silence fd 1 & 2 for
    // the whole load so none of it pollutes rwave's output. Restored on drop
    // (incl. early `?` returns); our own error strings are built here but
    // printed later by rwave, so they're unaffected.
    let _silence = StdioSilence::new();

    // libNPI.so calls shm_unlink but carries no DT_NEEDED on librt (confirmed
    // with readelf), so loading it cold fails outright with "undefined symbol:
    // shm_unlink" on any glibc older than 2.34, where shm_unlink still lived in
    // librt rather than libc.
    //
    // The preload must be RTLD_GLOBAL to have any effect: only a global-scope
    // object contributes symbols to later dlopens. `Library::new` would use
    // RTLD_LOCAL and silently do nothing at all here, so go through the unix
    // API explicitly. `forget` skips Drop, so dlclose never runs and the
    // library stays resident. Best-effort: on a newer glibc librt is a stub or
    // absent, and libNPI resolves shm_unlink from libc regardless.
    for name in ["librt.so.1", "librt.so"] {
        let opened = unsafe {
            libloading::os::unix::Library::open(
                Some(name),
                libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
            )
        };
        if let Ok(lib) = opened {
            std::mem::forget(lib);
            break;
        }
    }

    let lib = unsafe { Library::new(&path) }.map_err(|e| {
        bridge_err(ERR_PREFIX, format!("failed to load libNPI.so from {}: {}", path.display(), e))
    })?;

    macro_rules! sym {
        ($lib:expr, $mangled:expr, $sig:ty) => {{
            let s: libloading::Symbol<$sig> = unsafe { $lib.get($mangled) }.map_err(|e| {
                bridge_err(ERR_PREFIX, format!(
                    "missing NPI symbol {}: {}",
                    String::from_utf8_lossy($mangled),
                    e
                ))
            })?;
            *s
        }};
    }

    // npi_init(int& argc, char**& argv) — references are pointers across the
    // C ABI. NPI may retain argv/argc past the call, so they outlive the
    // process (leaked once). Called before any npi_fsdb_* API.
    let npi_init = sym!(
        lib,
        b"_Z8npi_initRiRPPc\0",
        unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int
    );




    let fsdb_open = sym!(lib, b"_Z13npi_fsdb_openPKc\0", unsafe extern "C" fn(*const c_char) -> NpiHandle);
    let fsdb_close = sym!(lib, b"_Z14npi_fsdb_closePv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let is_fsdb = sym!(lib, b"_Z16npi_fsdb_is_fsdbPKc\0", unsafe extern "C" fn(*const c_char) -> c_int);

    let file_property = sym!(lib, b"_Z22npi_fsdb_file_property23npiFsdbFilePropertyTypePvPi\0",
        unsafe extern "C" fn(c_int, NpiHandle, *mut c_int) -> c_int);
    let file_property_str = sym!(lib, b"_Z26npi_fsdb_file_property_str23npiFsdbFilePropertyTypePv\0",
        unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char);
    let min_time = sym!(lib, b"_Z17npi_fsdb_min_timePvPy\0", unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int);
    let max_time = sym!(lib, b"_Z17npi_fsdb_max_timePvPy\0", unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int);

    let iter_top_scope = sym!(lib, b"_Z23npi_fsdb_iter_top_scopePv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let iter_child_scope = sym!(lib, b"_Z25npi_fsdb_iter_child_scopePv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let iter_scope_next = sym!(lib, b"_Z24npi_fsdb_iter_scope_nextPv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let iter_scope_stop = sym!(lib, b"_Z24npi_fsdb_iter_scope_stopPv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let iter_sig = sym!(lib, b"_Z17npi_fsdb_iter_sigPv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let iter_sig_next = sym!(lib, b"_Z22npi_fsdb_iter_sig_nextPv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let iter_sig_stop = sym!(lib, b"_Z22npi_fsdb_iter_sig_stopPv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let scope_property_str = sym!(lib, b"_Z27npi_fsdb_scope_property_str24npiFsdbScopePropertyTypePv\0",
        unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char);
    let sig_property = sym!(lib, b"_Z21npi_fsdb_sig_property22npiFsdbSigPropertyTypePvPi\0",
        unsafe extern "C" fn(c_int, NpiHandle, *mut c_int) -> c_int);
    let sig_property_str = sym!(lib, b"_Z25npi_fsdb_sig_property_str22npiFsdbSigPropertyTypePv\0",
        unsafe extern "C" fn(c_int, NpiHandle) -> *const c_char);

    let create_vct = sym!(lib, b"_Z19npi_fsdb_create_vctPv\0", unsafe extern "C" fn(NpiHandle) -> NpiHandle);
    let release_vct = sym!(lib, b"_Z20npi_fsdb_release_vctPv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let goto_first = sym!(lib, b"_Z19npi_fsdb_goto_firstPv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let goto_next = sym!(lib, b"_Z18npi_fsdb_goto_nextPv\0", unsafe extern "C" fn(NpiHandle) -> c_int);
    let goto_time = sym!(lib, b"_Z18npi_fsdb_goto_timePvy\0", unsafe extern "C" fn(NpiHandle, NpiFsdbTime) -> c_int);
    let vct_time = sym!(lib, b"_Z17npi_fsdb_vct_timePvPy\0", unsafe extern "C" fn(NpiHandle, *mut NpiFsdbTime) -> c_int);
    let vct_value = sym!(lib, b"_Z18npi_fsdb_vct_valuePvP12npiFsdbValue\0", unsafe extern "C" fn(NpiHandle, *mut NpiFsdbValue) -> c_int);

    // One-shot init. NPI inspects argv[0] (GetModuleFileName) to locate the
    // running module, so it must be a *real* executable path — pass
    // /proc/self/exe, not a placeholder, or npi_init aborts. argc/argv are
    // leaked since NPI may retain them for the process lifetime. License /
    // resource failures are NPI's own and surface concretely at fsdb_open.
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "rwave".to_string());
    let argv0: *mut c_char = CString::new(exe.replace('\0', "?"))
        .unwrap_or_else(|_| CString::new("rwave").expect("static"))
        .into_raw();
    let argc: &'static mut c_int = Box::leak(Box::new(1));
    let argv_arr: &'static mut [*mut c_char] =
        Box::leak(vec![argv0, ptr::null_mut()].into_boxed_slice());
    let argv_holder: &'static mut *mut *mut c_char = Box::leak(Box::new(argv_arr.as_mut_ptr()));
    let _ = unsafe { npi_init(argc as *mut c_int, argv_holder as *mut *mut *mut c_char) };

    Ok(LibNpi {
        _library: lib,
        path,
        fsdb_open,
        fsdb_close,
        is_fsdb,
        file_property,
        file_property_str,
        min_time,
        max_time,
        iter_top_scope,
        iter_child_scope,
        iter_scope_next,
        iter_scope_stop,
        iter_sig,
        iter_sig_next,
        iter_sig_stop,
        scope_property_str,
        sig_property,
        sig_property_str,
        create_vct,
        release_vct,
        goto_first,
        goto_next,
        goto_time,
        vct_time,
        vct_value,
    })
}

fn locate_npi() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("RWAVE_FSDB_LIB") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(bridge_err(ERR_PREFIX, format!(
            "RWAVE_FSDB_LIB={} does not exist",
            path.display()
        )));
    }
    if let Some(dir) = self_dir() {
        let direct = dir.join(NPI_FILENAME);
        if direct.is_file() {
            return Ok(direct);
        }
    }
    Ok(PathBuf::from(NPI_FILENAME))
}

const NPI_FILENAME: &str = "libNPI.so";

/// Silence NPI's own writes to fd 1 & 2 until the returned guard is dropped.
///
/// Needed anywhere NPI is called outside the one-shot load: `npi_load_design`
/// prints progress and license chatter, which would corrupt `--json` output.
pub fn silence_stdio() -> Option<impl Drop> {
    StdioSilence::new()
}

/// RAII guard that redirects fd 1 & 2 to `/dev/null` for its lifetime, then
/// restores them on drop. Used to swallow NPI's `npi_init` banner / license
/// chatter without depending on any NPI-specific quiet flag.
struct StdioSilence {
    saved_out: c_int,
    saved_err: c_int,
    devnull: c_int,
}

impl StdioSilence {
    fn new() -> Option<Self> {
        unsafe extern "C" {
            fn open(path: *const c_char, flags: c_int) -> c_int;
            fn dup(fd: c_int) -> c_int;
            fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
        }
        const O_WRONLY: c_int = 1;
        unsafe {
            let devnull = open(c"/dev/null".as_ptr(), O_WRONLY);
            if devnull < 0 {
                return None;
            }
            let saved_out = dup(1);
            let saved_err = dup(2);
            dup2(devnull, 1);
            dup2(devnull, 2);
            Some(StdioSilence { saved_out, saved_err, devnull })
        }
    }
}

impl Drop for StdioSilence {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
            fn close(fd: c_int) -> c_int;
            fn fflush(stream: *mut c_void) -> c_int;
        }
        unsafe {
            // NPI writes its banner through *buffered* C stdio, so the bytes
            // sit in the FILE* buffer and would flush to the real stdout at
            // process exit. Flush all streams now, while fd 1 & 2 still point
            // at /dev/null, to discard them — then restore.
            fflush(ptr::null_mut());
            dup2(self.saved_out, 1);
            dup2(self.saved_err, 2);
            close(self.saved_out);
            close(self.saved_err);
            close(self.devnull);
        }
    }
}
