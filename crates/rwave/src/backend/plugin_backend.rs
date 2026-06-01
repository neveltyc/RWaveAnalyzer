// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Generic [`WaveformBackend`] forwarder backed by a dynamically loaded
//! plugin (`docs/PLUGIN.md`).
//!
//! This module knows nothing about any specific waveform format — it only
//! knows how to call a vtable that conforms to [`crate::plugin::ffi`].
//! Per-format implementations (the cdylib that opens the file, any vendor
//! binary it bundles, the wheel that ships them) live outside this
//! repository; rwave's public contract is the C header and this forwarder.
//!
//! ## Lifetime management
//!
//! Once a plugin shared library is dlopened, it stays mapped for the
//! lifetime of the process — `dlclose` would invalidate the cached vtable,
//! and re-`dlopen`ing on every file is wasteful even when safe. The cache
//! lives in [`LOADED_PLUGINS`].
//!
//! Each [`PluginBackend`] instance owns one `*mut RwaveSession` handle,
//! closed in `Drop`. The vtable behind it is borrowed from the cache;
//! that borrow is `'static` because cache entries are never removed.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{LazyLock, Mutex};

use libloading::Library;

use super::{
    BackendError, BackendSid, BitStr, FileFormat, RawValue, SignalTrace, Timescale, VarDecl,
    WaveformBackend,
};
use crate::format::ValueKind;
use crate::plugin::ffi::{
    file_format, RwaveBackend, RwaveBackendInit, RwaveSession as PluginHandle, RwaveValueKind,
    RwaveVarDecl, RWAVE_BACKEND_ABI_VERSION, RWAVE_BACKEND_SYMBOL,
};
use crate::plugin::loader::{locate_plugin, LoadError};

// ---------------------------------------------------------------------------
// Process-wide plugin cache
// ---------------------------------------------------------------------------

/// A successfully loaded plugin. Held for the process lifetime; the
/// `vtable` pointer is valid as long as `_library` is alive (which is
/// forever, since cache entries are never removed).
struct LoadedPlugin {
    _library: Library,
    vtable: *const RwaveBackend,
}

// SAFETY: `Library` is already `Send + Sync`. The raw vtable pointer is
// only ever read (after init has succeeded); the plugin contract states
// it lives for the process. The Mutex around the map enforces atomicity
// of the insert; subsequent shared reads of an inserted entry need no
// further synchronisation.
unsafe impl Send for LoadedPlugin {}
unsafe impl Sync for LoadedPlugin {}

static LOADED_PLUGINS: LazyLock<Mutex<HashMap<String, &'static LoadedPlugin>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Locate, dlopen, and validate the plugin for `format`. Caches the result
/// for the rest of the process. Returns the cached entry on subsequent calls.
fn load_or_get(format: &str) -> Result<&'static LoadedPlugin, LoadError> {
    {
        let cache = LOADED_PLUGINS.lock().expect("plugin cache poisoned");
        if let Some(entry) = cache.get(format) {
            return Ok(*entry);
        }
    }

    let path = locate_plugin(format)?;

    // SAFETY: `Library::new` calls dlopen on the given path. The plugin's
    // init function will run as part of dlopen if the cdylib declares any
    // constructors; we tolerate that.
    let library = unsafe { Library::new(&path) }.map_err(|e| LoadError::LoadFailed {
        msg: format!("failed to load {}: {}", path.display(), e),
    })?;

    // SAFETY: symbol is the C-ABI init function declared in the header.
    let init: libloading::Symbol<RwaveBackendInit> = unsafe {
        library
            .get(RWAVE_BACKEND_SYMBOL)
            .map_err(|e| LoadError::LoadFailed {
                msg: format!("{}: missing rwave_backend symbol ({})", path.display(), e),
            })?
    };

    // Call init.
    let mut err_out: *const c_char = std::ptr::null();
    // SAFETY: calling the plugin's exported init function as documented.
    let vtable_raw: *const RwaveBackend = unsafe { init(&mut err_out) };
    if vtable_raw.is_null() {
        let msg = if err_out.is_null() {
            "plugin init returned NULL with no diagnostic".to_string()
        } else {
            // SAFETY: per the header, err_out (on NULL return) is a
            // static string the plugin does not intend to free.
            unsafe { CStr::from_ptr(err_out) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(LoadError::LoadFailed { msg });
    }

    // SAFETY: vtable_raw is non-NULL per the check above.
    let vtable: &RwaveBackend = unsafe { &*vtable_raw };

    // Validate ABI version. Dedicated error variant so the message can
    // be specific — the remediation is reinstalling a wheel built
    // against rwave's expected ABI, not retrying or debugging dlopen.
    if vtable.abi_version != RWAVE_BACKEND_ABI_VERSION {
        return Err(LoadError::AbiMismatch {
            format: format.to_string(),
            plugin_abi: vtable.abi_version,
            rwave_abi: RWAVE_BACKEND_ABI_VERSION,
        });
    }

    // Validate non-NULL required entry points. We don't enumerate all,
    // but the common ones get a sanity check so a malformed plugin
    // fails closed.
    if vtable.open.is_none()
        || vtable.close.is_none()
        || vtable.free_err.is_none()
        || vtable.var_decls.is_none()
        || vtable.load_traces.is_none()
        || vtable.timescale.is_none()
    {
        return Err(LoadError::LoadFailed {
            msg: format!(
                "{}: plugin vtable has NULL required entry points",
                path.display()
            ),
        });
    }

    // Validate `name` matches the format we asked for.
    if vtable.name.is_null() {
        return Err(LoadError::LoadFailed {
            msg: format!("{}: plugin vtable name is NULL", path.display()),
        });
    }
    // SAFETY: vtable.name non-NULL per check; the plugin contract says
    // it is a NUL-terminated string living for the process.
    let name_str = unsafe { CStr::from_ptr(vtable.name) }.to_string_lossy();
    if name_str != format {
        return Err(LoadError::LoadFailed {
            msg: format!(
                "{}: plugin advertises format '{}' but rwave asked for '{}'",
                path.display(),
                name_str,
                format,
            ),
        });
    }

    // All checks passed. Promote to &'static via Box::leak — fine because
    // the cache entry is process-lifetime by design.
    let entry: &'static LoadedPlugin = Box::leak(Box::new(LoadedPlugin {
        _library: library,
        vtable: vtable_raw,
    }));

    let mut cache = LOADED_PLUGINS.lock().expect("plugin cache poisoned");
    // A racing thread may have inserted while we were initialising; if
    // so, prefer its entry and let our `entry` simply leak (process-end
    // cleans up). This keeps the vtable identity stable.
    if let Some(existing) = cache.get(format) {
        return Ok(*existing);
    }
    cache.insert(format.to_string(), entry);
    Ok(entry)
}

// ---------------------------------------------------------------------------
// PluginBackend: implements WaveformBackend by forwarding to the vtable
// ---------------------------------------------------------------------------

/// One open waveform file, behind a plugin vtable. The vtable itself is
/// `'static` (process-lifetime cached); only the [`PluginHandle`] needs
/// active cleanup, which happens in [`Drop`].
pub struct PluginBackend {
    plugin: &'static LoadedPlugin,
    handle: *mut PluginHandle,
    path: String,
    /// `date()` and `version_str()` from the plugin, copied to owned
    /// `String`s at open. The trait getters return `&str`, and the
    /// plugin's strings are valid for the lifetime of `handle`, but
    /// Rust can't see that through the FFI boundary — caching as owned
    /// makes the borrow checker happy without an extra round-trip per call.
    date_cache: String,
    version_cache: String,
    /// Per-signal value kind, indexed by backend sid. Populated lazily
    /// (first call to [`Self::var_decls`] or [`Self::load_traces`]) and
    /// cached, since the streaming emit callback needs the kind to
    /// decode value strings into the right [`RawValue`] variant.
    kind_cache: std::cell::RefCell<Option<HashMap<u64, ValueKind>>>,
}

impl PluginBackend {
    /// Open a file via the plugin registered for `format`. Falls through
    /// to the discovery + dlopen path on first call per process.
    pub fn open(path: &str, format: &str) -> Result<PluginBackend, BackendError> {
        let plugin = load_or_get(format).map_err(|e| BackendError::Open(e.to_string()))?;

        // SAFETY: vtable validated non-NULL on the required slots in
        // `load_or_get`.
        let vtable: &RwaveBackend = unsafe { &*plugin.vtable };

        let path_c = CString::new(path).map_err(|_| {
            BackendError::Open(format!("path contains interior NUL: {path}"))
        })?;
        let mut err_out: *mut c_char = std::ptr::null_mut();
        // SAFETY: open is validated non-NULL; we pass a valid C string
        // pointer and an out-pointer for the error slot.
        let handle = unsafe { (vtable.open.unwrap())(path_c.as_ptr(), &mut err_out) };
        if handle.is_null() {
            let msg = if err_out.is_null() {
                format!("plugin open returned NULL for {path}")
            } else {
                // SAFETY: per the contract, err_out on failure points at
                // a plugin-allocated NUL-terminated string we must release
                // via free_err.
                let msg = unsafe { CStr::from_ptr(err_out) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { (vtable.free_err.unwrap())(err_out) };
                msg
            };
            return Err(BackendError::Open(msg));
        }

        // Pull date / version from the plugin once and copy to owned
        // strings. The plugin owns the C-string buffers for the lifetime
        // of `handle`; we copy them out so we can hand back &str.
        let date_cache = unsafe { plugin_string(vtable.date, handle) };
        let version_cache = unsafe { plugin_string(vtable.version_str, handle) };

        Ok(PluginBackend {
            plugin,
            handle,
            path: path.to_string(),
            date_cache,
            version_cache,
            kind_cache: std::cell::RefCell::new(None),
        })
    }

    fn vtable(&self) -> &'static RwaveBackend {
        // SAFETY: validated on load; cache entry is &'static.
        unsafe { &*self.plugin.vtable }
    }

    /// Build (or return cached) sid → ValueKind map. Used by the trace
    /// emit trampoline to decode `value_buf` into the right [`RawValue`].
    fn ensure_kind_cache(&self) -> std::cell::Ref<'_, HashMap<u64, ValueKind>> {
        if self.kind_cache.borrow().is_none() {
            let decls = self.var_decls_raw();
            let mut map = HashMap::with_capacity(decls.len());
            for d in &decls {
                map.insert(d.backend_sid, d.kind);
            }
            *self.kind_cache.borrow_mut() = Some(map);
        }
        std::cell::Ref::map(self.kind_cache.borrow(), |c| c.as_ref().unwrap())
    }

    /// Direct vtable call for var_decls. Returns (sid_as_u64, kind) for
    /// each declaration, light enough for the kind_cache builder.
    fn var_decls_raw(&self) -> Vec<KindOnlyDecl> {
        let vtable = self.vtable();
        // SAFETY: var_decls validated non-NULL; cap=0 returns count.
        let total = unsafe { (vtable.var_decls.unwrap())(self.handle, std::ptr::null_mut(), 0) };
        if total == 0 {
            return Vec::new();
        }
        let mut buf: Vec<RwaveVarDecl> = Vec::with_capacity(total);
        let written =
            unsafe { (vtable.var_decls.unwrap())(self.handle, buf.as_mut_ptr(), total) };
        // Same defensive clamp as in `var_decls`: a misbehaving plugin
        // returning > total must not lead to set_len past capacity.
        let written = written.min(total);
        // SAFETY: written <= total == capacity.
        unsafe { buf.set_len(written) };

        buf.iter()
            .map(|d| KindOnlyDecl {
                backend_sid: d.backend_sid,
                kind: map_kind(d.kind),
            })
            .collect()
    }
}

impl Drop for PluginBackend {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let vtable = self.vtable();
            if let Some(close) = vtable.close {
                // SAFETY: close validated non-NULL on load; handle is ours.
                unsafe { close(self.handle) };
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

struct KindOnlyDecl {
    backend_sid: u64,
    kind: ValueKind,
}

// ---------------------------------------------------------------------------
// WaveformBackend impl
// ---------------------------------------------------------------------------

impl WaveformBackend for PluginBackend {
    fn path(&self) -> &str {
        &self.path
    }

    fn file_format(&self) -> FileFormat {
        let vtable = self.vtable();
        let f = match vtable.file_format {
            Some(f) => unsafe { f(self.handle) },
            None => file_format::UNKNOWN,
        };
        // Plugins reporting a non-built-in format value collapse to
        // Unknown — rwave does not maintain per-format constants for
        // plugin formats. Callers that need plugin format identity use
        // the vtable's `name` field instead.
        match f {
            file_format::VCD => FileFormat::Vcd,
            file_format::FST => FileFormat::Fst,
            file_format::GHW => FileFormat::Ghw,
            _ => FileFormat::Unknown,
        }
    }

    fn timescale(&self) -> Timescale {
        let vtable = self.vtable();
        let mut secs: f64 = 1.0;
        let mut display: *const c_char = std::ptr::null();
        // SAFETY: timescale validated non-NULL on load.
        unsafe { (vtable.timescale.unwrap())(self.handle, &mut secs, &mut display) };
        let display_str = if display.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(display) }.to_string_lossy().into_owned()
        };
        Timescale {
            seconds_per_tick: secs,
            display: display_str,
        }
    }

    fn date(&self) -> &str {
        &self.date_cache
    }

    fn version(&self) -> &str {
        &self.version_cache
    }

    fn comments(&self) -> Vec<String> {
        // ABI v1 has no comments accessor. Return empty.
        Vec::new()
    }

    fn var_decls(&self) -> Vec<VarDecl> {
        let vtable = self.vtable();
        // SAFETY: var_decls validated non-NULL; cap=0 returns count.
        let total = unsafe { (vtable.var_decls.unwrap())(self.handle, std::ptr::null_mut(), 0) };
        if total == 0 {
            return Vec::new();
        }
        let mut buf: Vec<RwaveVarDecl> = Vec::with_capacity(total);
        let written =
            unsafe { (vtable.var_decls.unwrap())(self.handle, buf.as_mut_ptr(), total) };
        // Clamp to capacity so a misbehaving plugin that returns a
        // larger count than it wrote can't drive `set_len` past the
        // allocation.
        let written = written.min(total);
        // SAFETY: written <= total == capacity; the plugin wrote
        // `written` valid items per the C ABI contract.
        unsafe { buf.set_len(written) };

        buf.iter()
            .map(|d| {
                let full = cstr_to_owned(d.full_path);
                let scope = cstr_to_owned(d.scope_path);
                let typ = if d.type_str.is_null() {
                    "wire"
                } else {
                    let s = unsafe { CStr::from_ptr(d.type_str) }.to_str().unwrap_or("wire");
                    map_type_str(s)
                };
                VarDecl {
                    full_path: full,
                    scope_path: scope,
                    width: d.width,
                    type_str: typ,
                    kind: map_kind(d.kind),
                    backend_sid: BackendSid(d.backend_sid as usize),
                }
            })
            .collect()
    }

    fn time_range(&self) -> Option<(i64, i64)> {
        let vtable = self.vtable();
        let mut lo: i64 = 0;
        let mut hi: i64 = 0;
        let rc = match vtable.time_range {
            Some(f) => unsafe { f(self.handle, &mut lo, &mut hi) },
            None => 0,
        };
        if rc == 0 { None } else { Some((lo, hi)) }
    }

    fn time_step_count(&self) -> usize {
        let vtable = self.vtable();
        match vtable.time_step_count {
            Some(f) => unsafe { f(self.handle) },
            None => 0,
        }
    }

    fn load_traces(&mut self, sids: &[BackendSid]) -> Vec<SignalTrace> {
        let vtable = self.vtable();
        let n = sids.len();

        // Pre-allocate per-output trace.
        let mut output: Vec<SignalTrace> = (0..n)
            .map(|_| SignalTrace {
                times: Vec::new(),
                values: Vec::new(),
            })
            .collect();

        if n == 0 {
            return output;
        }

        // sid → output index
        let mut idx_map: HashMap<u64, usize> = HashMap::with_capacity(n);
        for (i, sid) in sids.iter().enumerate() {
            idx_map.insert(sid.0 as u64, i);
        }

        // Borrow the kind cache for the duration of the call. We need
        // it to decode value_buf inside the emit trampoline.
        let kind_cache = self.ensure_kind_cache();

        let raw_sids: Vec<u64> = sids.iter().map(|s| s.0 as u64).collect();

        let mut ctx = EmitCtx {
            output: &mut output,
            idx_map: &idx_map,
            kinds: &kind_cache,
        };

        // SAFETY: load_traces validated non-NULL; we hand it owned
        // pointers and a ctx whose layout we control.
        let _rc = unsafe {
            (vtable.load_traces.unwrap())(
                self.handle,
                raw_sids.as_ptr(),
                raw_sids.len(),
                emit_trampoline,
                &mut ctx as *mut _ as *mut c_void,
            )
        };

        output
    }
}

// ---------------------------------------------------------------------------
// Emit trampoline
// ---------------------------------------------------------------------------

struct EmitCtx<'a> {
    output: &'a mut Vec<SignalTrace>,
    idx_map: &'a HashMap<u64, usize>,
    kinds: &'a HashMap<u64, ValueKind>,
}

/// C-ABI trampoline plugins call once per change event. Decodes the
/// value into the appropriate [`RawValue`] variant and appends to the
/// caller's `Vec<SignalTrace>`.
unsafe extern "C" fn emit_trampoline(
    ctx: *mut c_void,
    backend_sid: u64,
    time_tick: i64,
    value_buf: *const c_char,
    value_len: u32,
) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: ctx originates from a `&mut EmitCtx` cast; the cast is
    // round-trip-stable. Caller (PluginBackend::load_traces) holds the
    // borrow for the duration of the plugin call.
    let ctx = unsafe { &mut *(ctx as *mut EmitCtx<'_>) };

    let Some(&idx) = ctx.idx_map.get(&backend_sid) else {
        return;
    };
    let kind = ctx.kinds.get(&backend_sid).copied().unwrap_or(ValueKind::Bits);

    let value_str: &str = if value_buf.is_null() || value_len == 0 {
        ""
    } else {
        // SAFETY: value_buf valid for value_len bytes per the contract;
        // we treat as borrowed for this call only.
        let slice =
            unsafe { std::slice::from_raw_parts(value_buf as *const u8, value_len as usize) };
        std::str::from_utf8(slice).unwrap_or("")
    };

    let raw = match kind {
        ValueKind::Bits => {
            RawValue::Bits(BitStr::from_ascii_iter(value_str.len(), value_str.chars()))
        }
        ValueKind::Real => RawValue::Real(value_str.parse().unwrap_or(0.0)),
        ValueKind::Str => RawValue::Str(value_str.to_string()),
        ValueKind::Event => RawValue::Event,
    };

    let trace = &mut ctx.output[idx];
    trace.times.push(time_tick);
    trace.values.push(raw);
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn cstr_to_owned(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Call an optional vtable getter that returns a `*const c_char` and
/// convert the result to an owned `String`. Returns empty if the slot
/// is `None` or the plugin returns NULL.
///
/// # Safety
/// `getter` (when `Some`) must be a non-NULL function pointer; `handle`
/// must be the plugin handle the getter was attached to.
unsafe fn plugin_string(
    getter: Option<unsafe extern "C" fn(*mut PluginHandle) -> *const c_char>,
    handle: *mut PluginHandle,
) -> String {
    match getter {
        Some(f) => {
            // SAFETY: caller asserts f is non-NULL and handle valid.
            let p = unsafe { f(handle) };
            cstr_to_owned(p)
        }
        None => String::new(),
    }
}

fn map_kind(k: RwaveValueKind) -> ValueKind {
    match k {
        RwaveValueKind::Bits => ValueKind::Bits,
        RwaveValueKind::Real => ValueKind::Real,
        RwaveValueKind::Str => ValueKind::Str,
        RwaveValueKind::Event => ValueKind::Event,
    }
}

/// Map the plugin's `type_str` (any NUL-terminated UTF-8 string) into
/// the small, fixed, `&'static str` set rwave's domain layer expects.
/// Unknown values fall back to `"wire"`.
fn map_type_str(s: &str) -> &'static str {
    match s {
        "wire" => "wire",
        "reg" => "reg",
        "real" => "real",
        "realtime" => "realtime",
        "event" => "event",
        "integer" => "integer",
        "time" => "time",
        "parameter" => "parameter",
        "logic" => "logic",
        "bit" => "bit",
        "string" => "string",
        _ => "wire",
    }
}
