//! libloading bindings for Mentor libwlf.
//!
//! Mirrors the subset of `wlf_api.h` that the metadata, hierarchy, and
//! trace-decode paths exercise. libwlf is `wlfInit()`-once-per-process;
//! [`ensure_loaded`] does this lazily and skips `wlfCleanup` (OS reclaims).
//!
//! Library path resolution order:
//! 1. `$RWAVE_WLF_LIB` (absolute path)
//! 2. sibling of this cdylib (via `dladdr` / `GetModuleHandleEx`)
//! 3. platform loader default (`LD_LIBRARY_PATH` / `PATH`)

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::Library;

use super::ERR_PREFIX;
use crate::plugin::builtin::diag::bridge_err;
use crate::plugin::builtin::self_path::self_dir;

/// Mirror of `WlfFileInfo` from `wlf_api.h`. Field order, types, and
/// natural alignment must match the SysV x86_64 / MSVC x64 layout the
/// Mentor binary was compiled against. `#[repr(C)]` lets the Rust
/// compiler insert the same padding the C compiler did between
/// e.g. `product_name[128]` and `creation_time` (i64).
#[repr(C)]
#[derive(Debug)]
pub struct WlfFileInfo {
    pub last_time: i64,
    pub last_delta: c_int,
    pub signal_count: c_int,
    pub resolution_limit: c_int,
    pub api_version: [u8; 16],
    pub file_format_version: [u8; 16],
    pub product_name: [u8; 128],
    pub creation_time: i64,
    pub compressed: c_int,
    pub mti_version: [u8; 16],
    pub platform: [u8; 16],
    pub start_time: i64,
    pub start_delta: c_int,
    pub optimized: c_int,
    pub magic: i16,
}

impl Default for WlfFileInfo {
    fn default() -> Self {
        // SAFETY: the struct is all integer / byte-array fields with no
        // niche representations; zero is a valid bit pattern.
        unsafe { std::mem::zeroed() }
    }
}

/// Property selectors used in `wlfSymPropString` / `wlfSymPropInt` /
/// `wlfSymPropSymbolSel`. Subset; mirrors `WlfPropEnum`.
#[allow(dead_code)] // SYMBOL_FULLNAME is reserved for value-decode work; not yet used
pub mod prop {
    pub const SYMBOL_NAME: i32 = 0;
    pub const SYMBOL_FULLNAME: i32 = 1;
    pub const SYMBOL_TYPE: i32 = 7;
    pub const ARCHIVE_NUMBER: i32 = 11;
    pub const TYPE_ID: i32 = 12;
}

/// Type-property selectors used in `wlfTypePropInt`. Subset of
/// `WlfTypePropEnum` from `wlf_api.h`.
pub mod type_prop {
    pub const REGISTER_WIDTH: i32 = 7;
    pub const ARRAY_LENGTH: i32 = 3;
    pub const VALUE_SIZE: i32 = 9;
}

/// `WlfRadixEnum` values. Used by [`LibWlf::wlf_value_to_string`].
pub mod radix {
    pub const DEFAULT: i32 = 1;
    pub const BINARY: i32 = 2;
}

/// `WlfCallbackRequest` values. Used by [`LibWlf::wlf_append_signal_event_cb`].
pub mod callback_request {
    /// Callback fires immediately on primitive value change. Required to
    /// force libwlf to actually iterate simulation time (without an
    /// IMMEDIATE signal CB, libwlf optimises out the whole scan).
    pub const IMMEDIATE: i32 = 0;
}

/// `WlfCallbackResponse` values returned by signal/time callbacks.
#[allow(dead_code)] // STOP is reserved for early-exit support
pub mod callback_response {
    pub const CONTINUE: i32 = 0;
    pub const STOP: i32 = 1;
}

/// `WlfSymbolSel` bit masks for scope detection. Mirrors the
/// `wlfSel*` macros in `wlf_api.h`. We only need the "is this a scope?"
/// classification here — full sel decoding lives in higher layers.
pub mod sel {
    pub const ARCHITECTURE: u32 = 0x0000_0002;
    pub const BLOCK: u32 = 0x0000_0004;
    pub const GENERATE: u32 = 0x0000_0008;
    pub const PACKAGE: u32 = 0x0000_0010;
    pub const FOREIGN: u32 = 0x0000_0020;
    pub const PROCESS: u32 = 0x0000_0040;
    pub const VL_GENERATE_BLOCK: u32 = 0x0000_0080;
    pub const MODULE: u32 = 0x0000_8000;
    pub const PRIMITIVE: u32 = 0x0001_0000;
    pub const TASK: u32 = 0x0002_0000;
    pub const FUNCTION: u32 = 0x0004_0000;
    pub const STATEMENT: u32 = 0x0008_0000;
    pub const VIRTUAL_REGION: u32 = 0x0010_0000;

    pub const REAL: u32 = 0x0400_0000;
    pub const NAMED_EVENT: u32 = 0x2000_0000;

    pub const SCOPE_MASK: u32 = ARCHITECTURE
        | BLOCK
        | GENERATE
        | PACKAGE
        | FOREIGN
        | PROCESS
        | VL_GENERATE_BLOCK
        | MODULE
        | PRIMITIVE
        | TASK
        | FUNCTION
        | STATEMENT
        | VIRTUAL_REGION;

    pub fn is_scope(sel: u32) -> bool {
        (sel & SCOPE_MASK) != 0
    }
}

/// The bundle of libwlf entry points the bridge needs. Stored as raw
/// `extern "C" fn` pointers extracted from libloading symbols — the
/// owning [`Library`] is held alongside so the symbols stay valid.
///
/// Some fields are loaded but not yet wired through to the ABI surface
/// (notably the trace-decode entry points and `wlfInit`, which is called
/// once during [`load_libwlf_once`] and then never again). They are
/// resolved upfront so that adding the load_traces implementation is a
/// pure higher-layer change.
#[allow(dead_code)]
pub struct LibWlf {
    _library: Library,

    pub wlf_init: unsafe extern "C" fn() -> c_int,
    pub wlf_file_open: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    pub wlf_file_close: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub wlf_file_info: unsafe extern "C" fn(*mut c_void, *mut WlfFileInfo) -> c_int,
    pub wlf_file_diag: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    pub wlf_file_get_top_region: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub wlf_file_num_sigs: unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int,
    pub wlf_file_end_time: unsafe extern "C" fn(*mut c_void, *mut i64) -> c_int,
    pub wlf_file_resolution: unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int,

    pub wlf_sym_prop_string: unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char,
    pub wlf_sym_prop_int: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    pub wlf_sym_prop_symbol_sel: unsafe extern "C" fn(*mut c_void, c_int) -> c_uint,
    pub wlf_sym_prop_type_id: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    pub wlf_type_prop_int: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,

    pub wlf_sym_children: unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void,
    pub wlf_iterate: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub wlf_iterator_destroy: unsafe extern "C" fn(*mut c_void) -> c_int,

    // -- trace decode --
    pub wlf_pack_create: unsafe extern "C" fn() -> *mut c_void,
    pub wlf_pack_destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub wlf_value_create: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    pub wlf_value_destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub wlf_value_to_string:
        unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *const c_char,
    pub wlf_append_signal_event_cb: unsafe extern "C" fn(
        *mut c_void,                 // packId
        *mut c_void,                 // symId
        *mut c_void,                 // valueId
        c_int,                       // request (IMMEDIATE/...)
        SignalEventCB,               // cb function
        *mut c_void,                 // cbData
        *mut *mut c_void,            // cbPtr out — uses existing if non-NULL
    ) -> c_int,
    pub wlf_read_data_over_range: unsafe extern "C" fn(
        *mut c_void,                 // packId
        i64,                         // startTime
        c_int,                       // startDelta
        i64,                         // endTime
        c_int,                       // endDelta
        TimeAdvanceCB,               // time CB
        *mut c_void,                 // delta CB (can be NULL — declared opaque
                                     // so the Rust ABI doesn't fuss about
                                     // None for a fn-pointer slot)
        *mut c_void,                 // cb_data passed to both time/delta CBs
    ) -> c_int,
}

/// Signal-change callback signature. Matches `WlfSignalEventCB`.
pub type SignalEventCB = unsafe extern "C" fn(cb_data: *mut c_void, reason: c_int) -> c_int;

/// Time-advance callback signature. Matches `WlfTimeAdvanceCB`.
pub type TimeAdvanceCB =
    unsafe extern "C" fn(cb_data: *mut c_void, new_time: i64, new_delta: c_int) -> c_int;

// SAFETY: `Library` is Send + Sync. Function pointers are POD. The
// init function is called exactly once and the pointers remain valid
// for the process lifetime.
unsafe impl Send for LibWlf {}
unsafe impl Sync for LibWlf {}

/// Result of the one-time libwlf load + init. `Ok` means library is
/// loaded and `wlfInit()` returned 0; `Err` carries the diagnostic.
static LIBWLF: OnceLock<Result<LibWlf, String>> = OnceLock::new();

/// Load libwlf if not yet loaded; cached. Returns `Ok(())` whether the
/// load was fresh or already cached. Re-attempts are NOT made — if the
/// first attempt failed (e.g. env var pointed somewhere wrong), the
/// process must be restarted with corrected configuration.
pub fn ensure_loaded() -> Result<(), String> {
    let r = LIBWLF.get_or_init(load_libwlf_once);
    match r {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }
}

/// Borrow the cached library bundle. Panics if [`ensure_loaded`] has
/// not run successfully — call sites guarantee this via the ABI init
/// gating in [`crate::rwave_backend`].
pub fn libwlf() -> &'static LibWlf {
    LIBWLF
        .get()
        .and_then(|r| r.as_ref().ok())
        .expect("libwlf accessed before successful ensure_loaded()")
}

fn load_libwlf_once() -> Result<LibWlf, String> {
    let path = locate_libwlf()?;

    // SAFETY: dlopen is intrinsically unsafe (runs the library's ctor);
    // we trust Mentor's binary not to misbehave at load.
    let lib = unsafe { Library::new(&path) }.map_err(|e| {
        bridge_err(ERR_PREFIX, format!("failed to load libwlf from {}: {}", path.display(), e))
    })?;

    macro_rules! sym {
        ($lib:expr, $name:expr, $sig:ty) => {{
            let s: libloading::Symbol<$sig> = unsafe { $lib.get($name) }
                .map_err(|e| bridge_err(ERR_PREFIX, format!("missing symbol {}: {}", String::from_utf8_lossy($name), e)))?;
            *s
        }};
    }

    let wlf_init = sym!(lib, b"wlfInit\0", unsafe extern "C" fn() -> c_int);
    let wlf_file_open = sym!(
        lib,
        b"wlfFileOpen\0",
        unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void
    );
    let wlf_file_close = sym!(lib, b"wlfFileClose\0", unsafe extern "C" fn(*mut c_void) -> c_int);
    let wlf_file_info = sym!(
        lib,
        b"wlfFileInfo\0",
        unsafe extern "C" fn(*mut c_void, *mut WlfFileInfo) -> c_int
    );
    let wlf_file_diag = sym!(
        lib,
        b"wlfFileDiag\0",
        unsafe extern "C" fn(*mut c_void) -> *const c_char
    );
    let wlf_file_get_top_region = sym!(
        lib,
        b"wlfFileGetTopRegion\0",
        unsafe extern "C" fn(*mut c_void) -> *mut c_void
    );
    let wlf_file_num_sigs = sym!(
        lib,
        b"wlfFileNumSigs\0",
        unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int
    );
    let wlf_file_end_time = sym!(
        lib,
        b"wlfFileEndTime\0",
        unsafe extern "C" fn(*mut c_void, *mut i64) -> c_int
    );
    let wlf_file_resolution = sym!(
        lib,
        b"wlfFileResolution\0",
        unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int
    );

    let wlf_sym_prop_string = sym!(
        lib,
        b"wlfSymPropString\0",
        unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char
    );
    let wlf_sym_prop_int = sym!(
        lib,
        b"wlfSymPropInt\0",
        unsafe extern "C" fn(*mut c_void, c_int) -> c_int
    );
    let wlf_sym_prop_symbol_sel = sym!(
        lib,
        b"wlfSymPropSymbolSel\0",
        unsafe extern "C" fn(*mut c_void, c_int) -> c_uint
    );
    let wlf_sym_prop_type_id = sym!(
        lib,
        b"wlfSymPropTypeId\0",
        unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void
    );
    let wlf_type_prop_int = sym!(
        lib,
        b"wlfTypePropInt\0",
        unsafe extern "C" fn(*mut c_void, c_int) -> c_int
    );

    let wlf_sym_children = sym!(
        lib,
        b"wlfSymChildren\0",
        unsafe extern "C" fn(*mut c_void, c_uint) -> *mut c_void
    );
    let wlf_iterate = sym!(
        lib,
        b"wlfIterate\0",
        unsafe extern "C" fn(*mut c_void) -> *mut c_void
    );
    let wlf_iterator_destroy = sym!(
        lib,
        b"wlfIteratorDestroy\0",
        unsafe extern "C" fn(*mut c_void) -> c_int
    );

    let wlf_pack_create = sym!(lib, b"wlfPackCreate\0", unsafe extern "C" fn() -> *mut c_void);
    let wlf_pack_destroy = sym!(
        lib,
        b"wlfPackDestroy\0",
        unsafe extern "C" fn(*mut c_void) -> c_int
    );
    let wlf_value_create = sym!(
        lib,
        b"wlfValueCreate\0",
        unsafe extern "C" fn(*mut c_void) -> *mut c_void
    );
    let wlf_value_destroy = sym!(
        lib,
        b"wlfValueDestroy\0",
        unsafe extern "C" fn(*mut c_void) -> c_int
    );
    let wlf_value_to_string = sym!(
        lib,
        b"wlfValueToString\0",
        unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *const c_char
    );
    let wlf_append_signal_event_cb = sym!(
        lib,
        b"wlfAppendSignalEventCB\0",
        unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            c_int,
            SignalEventCB,
            *mut c_void,
            *mut *mut c_void,
        ) -> c_int
    );
    let wlf_read_data_over_range = sym!(
        lib,
        b"wlfReadDataOverRange\0",
        unsafe extern "C" fn(
            *mut c_void,
            i64,
            c_int,
            i64,
            c_int,
            TimeAdvanceCB,
            *mut c_void,
            *mut c_void,
        ) -> c_int
    );

    // One-shot init. Mentor's contract: must be exactly one wlfInit()
    // per process before any wlfFileOpen.
    let rc = unsafe { wlf_init() };
    if rc != 0 {
        return Err(bridge_err(ERR_PREFIX, format!("wlfInit returned {rc}")));
    }

    Ok(LibWlf {
        _library: lib,
        wlf_init,
        wlf_file_open,
        wlf_file_close,
        wlf_file_info,
        wlf_file_diag,
        wlf_file_get_top_region,
        wlf_file_num_sigs,
        wlf_file_end_time,
        wlf_file_resolution,
        wlf_sym_prop_string,
        wlf_sym_prop_int,
        wlf_sym_prop_symbol_sel,
        wlf_sym_prop_type_id,
        wlf_type_prop_int,
        wlf_sym_children,
        wlf_iterate,
        wlf_iterator_destroy,
        wlf_pack_create,
        wlf_pack_destroy,
        wlf_value_create,
        wlf_value_destroy,
        wlf_value_to_string,
        wlf_append_signal_event_cb,
        wlf_read_data_over_range,
    })
}

fn locate_libwlf() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("RWAVE_WLF_LIB") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        return Err(bridge_err(ERR_PREFIX, format!(
            "RWAVE_WLF_LIB={} does not exist or is not a file",
            path.display()
        )));
    }

    // Look next to the rwave binary (and a `_vendor/` sibling): a user can
    // drop the vendor library beside the executable instead of setting the
    // env var. Name: `libwlf.so` (linux) / `libwlf.dll` (windows).
    if let Some(dir) = self_dir() {
        let name = default_libname();
        let direct = dir.join(name);
        if direct.is_file() {
            return Ok(direct);
        }
        let in_vendor = dir.join("_vendor").join(name);
        if in_vendor.is_file() {
            return Ok(in_vendor);
        }
    }

    // Fall through to the platform default; libloading delegates to
    // dlopen / LoadLibrary, which honours LD_LIBRARY_PATH / PATH.
    Ok(PathBuf::from(default_libname()))
}

#[cfg(target_os = "windows")]
fn default_libname() -> &'static str {
    "libwlf.dll"
}

#[cfg(not(target_os = "windows"))]
fn default_libname() -> &'static str {
    "libwlf.so"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_scope_recognises_module() {
        assert!(sel::is_scope(sel::MODULE));
        assert!(sel::is_scope(sel::BLOCK));
        assert!(sel::is_scope(sel::ARCHITECTURE));
        assert!(sel::is_scope(sel::PACKAGE));
    }

    #[test]
    fn is_scope_rejects_pure_signal_bits() {
        // Bare signal-like bits (REAL, NAMED_EVENT) are leaves.
        assert!(!sel::is_scope(sel::REAL));
        assert!(!sel::is_scope(sel::NAMED_EVENT));
        assert!(!sel::is_scope(0));
    }

    #[test]
    fn is_scope_treats_mixed_mask_as_scope() {
        // If a symbol carries any scope bit, classify as scope.
        assert!(sel::is_scope(sel::MODULE | sel::REAL));
    }

    #[test]
    fn default_libname_is_platform_appropriate() {
        let name = default_libname();
        if cfg!(target_os = "windows") {
            assert!(name.ends_with(".dll"));
        } else {
            assert!(name.ends_with(".so"));
        }
    }

    #[test]
    fn wlf_file_info_layout_size_is_finite() {
        // We don't pin the exact byte count (depends on padding rules)
        // but it must be larger than the sum of explicit field bytes,
        // which is well over zero.
        assert!(std::mem::size_of::<WlfFileInfo>() >= 200);
    }
}
