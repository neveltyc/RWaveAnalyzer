// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Generic [`WaveformBackend`] forwarder backed by a dynamically loaded
//! plugin (`docs/PLUGIN.md`).
//!
//! This module knows nothing about any specific waveform format — it only
//! knows how to call a vtable that conforms to [`crate::plugin::ffi`]. The
//! vtable comes from a compiled-in built-in ([`crate::plugin::builtin`]) or
//! an external plugin `.so` named by `$RWAVE_PLUGIN_<EXT>`; either way this
//! forwarder is the single adapter. rwave's public contract is the C header.
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
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{LazyLock, Mutex};

use libloading::Library;

use super::{
    BackendError, BackendSid, BitStr, FileFormat, RawValue, SignalTrace, Timescale, VarDecl,
    WaveformBackend,
};
use crate::format::ValueKind;
use crate::plugin::builtin::{self, BuiltinError};
use crate::plugin::ffi::{
    file_format, RwaveBackend, RwaveBackendInit, RwaveEmit, RwaveSession as PluginHandle,
    RwaveValueKind, RwaveVarDecl, RWAVE_BACKEND_ABI_VERSION, RWAVE_BACKEND_SYMBOL,
};
use crate::plugin::loader::{external_plugin_path, LoadError};

// ---------------------------------------------------------------------------
// Process-wide plugin cache
// ---------------------------------------------------------------------------

/// A resolved backend, external or built-in. Held for the process
/// lifetime; the `vtable` pointer stays valid because cache entries are
/// never removed — an external plugin additionally keeps `library`
/// mapped, while a built-in's vtable is `&'static` to begin with.
struct LoadedPlugin {
    /// `Some` for an external (dlopened) plugin — keeps the shared library
    /// mapped so the vtable behind it stays valid. `None` for a built-in,
    /// whose vtable is compiled into the rwave binary. Also read as the
    /// built-in/external discriminator by [`LoadedPlugin::is_builtin`].
    // Only the fsdb design-query path reads this; elsewhere it is held purely
    // to keep the library mapped.
    #[cfg_attr(
        not(all(feature = "fsdb", target_os = "linux", target_arch = "x86_64")),
        allow(dead_code)
    )]
    library: Option<Library>,
    vtable: *const RwaveBackend,
}

impl LoadedPlugin {
    /// Whether this vtable is compiled into rwave rather than dlopened.
    ///
    /// The distinction matters because capabilities beyond the C ABI (design
    /// queries) exist only for built-ins, whose concrete session type this
    /// crate knows. An external plugin advertising the same format token is
    /// still a different implementation behind an opaque handle.
    #[cfg(all(feature = "fsdb", target_os = "linux", target_arch = "x86_64"))]
    fn is_builtin(&self) -> bool {
        self.library.is_none()
    }
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

/// Resolve, validate, and cache the backend for `format`, returning the
/// cached entry on every later call. Resolution order:
///
/// 1. **external override** — `$RWAVE_PLUGIN_<EXT>` names a `.so` → dlopen it;
/// 2. **built-in** — `wlf`/`fsdb` compiled into this build;
/// 3. otherwise a clean "no backend" error.
///
/// An external override wins over a built-in of the same extension, so an
/// external `.fsdb` backend can supersede the built-in NPI one.
fn load_or_get(format: &str) -> Result<&'static LoadedPlugin, LoadError> {
    {
        let cache = LOADED_PLUGINS.lock().expect("plugin cache poisoned");
        if let Some(entry) = cache.get(format) {
            return Ok(*entry);
        }
    }

    // 1. External override.
    if let Some(path) = external_plugin_path(format) {
        return load_external(format, &path);
    }

    // 2. Compiled-in built-in (runs the backend's one-time vendor-lib load).
    match builtin::vtable(format) {
        Ok(vtable) => return register(format, None, vtable as *const RwaveBackend),
        Err(BuiltinError::InitFailed(msg)) => return Err(LoadError::LoadFailed { msg }),
        Err(BuiltinError::Unavailable) => {
            return Err(LoadError::BuiltinUnavailable {
                format: format.to_string(),
                platforms: builtin::supported_platforms(format),
            });
        }
        Err(BuiltinError::NotBuiltin) => {}
    }

    // 3. Nothing handles this extension.
    Err(LoadError::NoBackend {
        format: format.to_string(),
    })
}

/// dlopen an external backend `.so`, resolve and call its `rwave_backend`
/// init, then hand the resulting vtable to [`register`].
fn load_external(format: &str, path: &std::path::Path) -> Result<&'static LoadedPlugin, LoadError> {
    // SAFETY: `Library::new` calls dlopen on the given path. The plugin's
    // init function runs as part of dlopen if the cdylib declares any
    // constructors; we tolerate that.
    let library = unsafe { Library::new(path) }.map_err(|e| LoadError::LoadFailed {
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

    let mut err_out: *const c_char = std::ptr::null();
    // SAFETY: calling the plugin's exported init function as documented.
    let vtable_raw: *const RwaveBackend = unsafe { init(&mut err_out) };
    if vtable_raw.is_null() {
        let msg = if err_out.is_null() {
            "plugin init returned NULL with no diagnostic".to_string()
        } else {
            // SAFETY: per the header, err_out (on NULL return) is a static
            // string the plugin does not intend to free.
            unsafe { CStr::from_ptr(err_out) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(LoadError::LoadFailed { msg });
    }

    register(format, Some(library), vtable_raw)
}

/// Validate a vtable (ABI version, required slots, `name` match) and insert
/// it into the process-wide cache. `library` is `Some` for an external
/// plugin (kept mapped) and `None` for a built-in. Promotes to `&'static`
/// via `Box::leak` — sound because cache entries are process-lifetime.
fn register(
    format: &str,
    library: Option<Library>,
    vtable_raw: *const RwaveBackend,
) -> Result<&'static LoadedPlugin, LoadError> {
    // SAFETY: vtable_raw is non-NULL — a built-in vtable is `&'static`, and
    // the external path NULL-checks before calling here.
    let vtable: &RwaveBackend = unsafe { &*vtable_raw };

    // ABI version. Dedicated variant so the message names the remediation
    // (rebuild the backend) rather than a dlopen retry. A built-in never
    // mismatches — it is compiled against this same `ffi` module.
    if vtable.abi_version != RWAVE_BACKEND_ABI_VERSION {
        return Err(LoadError::AbiMismatch {
            format: format.to_string(),
            plugin_abi: vtable.abi_version,
            rwave_abi: RWAVE_BACKEND_ABI_VERSION,
        });
    }

    // Required entry points. A malformed vtable fails closed rather than
    // risking UB on first call.
    if vtable.open.is_none()
        || vtable.close.is_none()
        || vtable.free_err.is_none()
        || vtable.var_decls.is_none()
        || vtable.load_traces.is_none()
        || vtable.timescale.is_none()
    {
        return Err(LoadError::LoadFailed {
            msg: format!("{format}: backend vtable has NULL required entry points"),
        });
    }

    // `name` must match the format we asked for.
    if vtable.name.is_null() {
        return Err(LoadError::LoadFailed {
            msg: format!("{format}: backend vtable name is NULL"),
        });
    }
    // SAFETY: vtable.name non-NULL per check; the contract says it is a
    // NUL-terminated string living for the process.
    let name_str = unsafe { CStr::from_ptr(vtable.name) }.to_string_lossy();
    if name_str != format {
        return Err(LoadError::LoadFailed {
            msg: format!("backend advertises format '{name_str}' but rwave asked for '{format}'"),
        });
    }

    // All checks passed. Promote to &'static via Box::leak — fine because
    // the cache entry is process-lifetime by design.
    let entry: &'static LoadedPlugin = Box::leak(Box::new(LoadedPlugin {
        library,
        vtable: vtable_raw,
    }));

    let mut cache = LOADED_PLUGINS.lock().expect("plugin cache poisoned");
    // A racing thread may have inserted while we resolved; prefer its entry
    // and let ours leak (process-end cleans up) to keep vtable identity
    // stable.
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

    /// Shared driver for full and windowed trace decode. Sets up the per-sid
    /// output vector, the sid→index map, the kind cache, and the emit context,
    /// then hands `(sids_ptr, n, emit, ctx)` to `invoke` — which calls whichever
    /// vtable entry (`load_traces` or `load_traces_windowed`) applies. Returns
    /// one [`SignalTrace`] per input sid, in order.
    fn run_trace_decode<I>(&self, sids: &[BackendSid], invoke: I) -> Vec<SignalTrace>
    where
        I: FnOnce(*const u64, usize, RwaveEmit, *mut c_void),
    {
        let n = sids.len();
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

        // Borrow the kind cache for the duration of the call. We need it to
        // decode value_buf inside the emit trampoline.
        let kind_cache = self.ensure_kind_cache();

        let raw_sids: Vec<u64> = sids.iter().map(|s| s.0 as u64).collect();

        let mut ctx = EmitCtx {
            output: &mut output,
            idx_map: &idx_map,
            kinds: &kind_cache,
        };

        invoke(
            raw_sids.as_ptr(),
            raw_sids.len(),
            emit_trampoline,
            &mut ctx as *mut _ as *mut c_void,
        );

        output
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
        let handle = self.handle;
        self.run_trace_decode(sids, |ptr, len, emit, ctx| {
            // SAFETY: load_traces validated non-NULL on load; we hand it owned
            // pointers and a ctx whose layout we control.
            let rc = unsafe { (vtable.load_traces.unwrap())(handle, ptr, len, emit, ctx) };
            warn_partial_decode(rc);
        })
    }

    fn supports_windowed(&self) -> bool {
        // Specialized iff the plugin filled the (optional, appended) vtable
        // slot. Both built-ins (NPI FSDB, WLF) do; external plugins predating
        // the slot leave it NULL.
        self.vtable().load_traces_windowed.is_some()
    }

    /// Design queries for the built-in Verdi NPI FSDB backend only.
    ///
    /// This capability is intentionally *not* in the C vtable. `RwaveBackend`
    /// is a C struct whose length is fixed when a plugin is compiled, so a
    /// newer host that appends slots reads past the end of an older plugin's
    /// object. Routing the capability through the concrete Rust type instead
    /// keeps the ABI frozen and costs external plugins nothing.
    ///
    /// The guard is both halves of the identity: `is_builtin` rules out an
    /// external `.so`, and the format name rules out the other built-in (WLF).
    /// An external FSDB plugin selected via `$RWAVE_PLUGIN_FSDB` is therefore
    /// excluded automatically — which is also correct on the merits, since the
    /// FFR reader it wraps has no connectivity API at all.
    #[cfg(all(feature = "fsdb", target_os = "linux", target_arch = "x86_64"))]
    fn design_query(&mut self) -> Option<&mut dyn super::DesignQuery> {
        use crate::plugin::builtin::fsdb::backend::FsdbBackend;
        if !self.plugin.is_builtin() {
            return None;
        }
        // SAFETY: `name` is validated non-NULL and equal to the requested
        // format token in `register`.
        let name = unsafe { CStr::from_ptr(self.vtable().name) };
        if name.to_bytes() != b"fsdb" {
            return None;
        }
        // SAFETY: for the built-in fsdb backend the opaque session handle is
        // exactly the `Box::into_raw(Box::new(FsdbBackend))` produced by
        // `fsdb::api_open` — the same cast its own trampolines perform. The
        // two conditions above prove that provenance: only `builtin::vtable`
        // registers a built-in, and only `fsdb::vtable()` names itself "fsdb".
        // Tied to `&mut self`, so no second reference to the session can exist
        // while this one is alive.
        Some(unsafe { &mut *(self.handle as *mut FsdbBackend) })
    }

    fn load_traces_windowed(
        &mut self,
        sids: &[BackendSid],
        from: i64,
        to: Option<i64>,
    ) -> Vec<SignalTrace> {
        let vtable = self.vtable();
        let handle = self.handle;
        let Some(windowed) = vtable.load_traces_windowed else {
            // No windowed entry: a full decode is a correct (unoptimized)
            // answer. Callers gate on `supports_windowed`, so this is only a
            // safety net.
            return self.load_traces(sids);
        };
        // `None` upper edge maps to the ABI's INT64_MAX "to the end" sentinel.
        let to_tick = to.unwrap_or(i64::MAX);
        self.run_trace_decode(sids, |ptr, len, emit, ctx| {
            // SAFETY: `windowed` is non-NULL (matched `Some`); same pointer and
            // ctx contract as load_traces, plus the two tick bounds.
            let rc = unsafe { windowed(handle, ptr, len, from, to_tick, emit, ctx) };
            warn_partial_decode(rc);
        })
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
/// value into the appropriate [`RawValue`] variant and folds it into the
/// caller's `Vec<SignalTrace>` via [`fold_change`].
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

    fold_change(&mut ctx.output[idx], kind, time_tick, raw);
}

/// Fold one emitted change into a trace: at most one entry per tick and no
/// consecutive equal values. The duplicate suppression matches the wellen
/// decode; the per-tick collapse is stricter — wellen keeps same-tick
/// distinct values (a VCD glitch stays visible), while here the last write
/// per tick wins and a tick whose net value equals the previous entry's is
/// no change at all.
///
/// The stricter rule is deliberate: the vendor libraries behind this
/// trampoline report transport granularity, not user-visible writes — libwlf
/// delivers a wide vector one 32-bit word per callback (each carrying the
/// full, partially-updated vector) and NPI can deliver same-tick glitch VCs
/// — and the vendors' own tools display the collapsed net value. Without
/// this, a 256-bit bus counted 8 "changes" per real transition in `summary`
/// and printed 8 rows (7 of them transient partial values) per instant in
/// `dump`.
///
/// Events are exempt: every occurrence is meaningful, none carry values.
fn fold_change(trace: &mut SignalTrace, kind: ValueKind, tick: i64, raw: RawValue) {
    if kind == ValueKind::Event {
        trace.times.push(tick);
        trace.values.push(raw);
        return;
    }
    match trace.times.last() {
        Some(&last_t) if last_t == tick => {
            let n = trace.values.len();
            trace.values[n - 1] = raw;
            // Net value equal to the entry before → the tick is a no-change.
            if n >= 2 && values_equal(&trace.values[n - 1], &trace.values[n - 2]) {
                trace.times.pop();
                trace.values.pop();
            }
        }
        Some(_) if values_equal(trace.values.last().expect("non-empty"), &raw) => {}
        _ => {
            trace.times.push(tick);
            trace.values.push(raw);
        }
    }
}

/// Value equality as the wellen decode's duplicate check sees it: reals by
/// bit pattern (NaN == NaN, 0.0 != -0.0), everything else by content. Same
/// rule as the FST windowed reader.
fn values_equal(a: &RawValue, b: &RawValue) -> bool {
    match (a, b) {
        (RawValue::Real(x), RawValue::Real(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// A nonzero backend return means the decode stopped partway; the traces
/// collected so far are consumed regardless (the trait has no error channel),
/// so say so instead of presenting them as complete. The backend has already
/// printed its own diagnostic.
fn warn_partial_decode(rc: c_int) {
    if rc != 0 {
        eprintln!("rwave: backend trace decode reported rc={rc}; results may be incomplete");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(s: &str) -> RawValue {
        RawValue::Bits(BitStr::new(s))
    }

    fn trace() -> SignalTrace {
        SignalTrace {
            times: Vec::new(),
            values: Vec::new(),
        }
    }

    fn shape(t: &SignalTrace) -> Vec<(i64, String)> {
        t.times
            .iter()
            .zip(t.values.iter())
            .map(|(t, v)| (*t, v.raw_str().into_owned()))
            .collect()
    }

    #[test]
    fn fold_distinct_ticks_append() {
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 1, bits("0"));
        fold_change(&mut t, ValueKind::Bits, 5, bits("1"));
        assert_eq!(shape(&t), vec![(1, "0".into()), (5, "1".into())]);
    }

    #[test]
    fn fold_same_tick_last_wins() {
        // libwlf word-partial delivery: three callbacks at one tick, each a
        // fuller update; only the final value survives.
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 1, bits("0000"));
        fold_change(&mut t, ValueKind::Bits, 7, bits("0011"));
        fold_change(&mut t, ValueKind::Bits, 7, bits("1111"));
        assert_eq!(shape(&t), vec![(1, "0000".into()), (7, "1111".into())]);
    }

    #[test]
    fn fold_same_tick_net_nochange_disappears() {
        // Glitch: value pulses and returns within one tick. The net entry
        // equals the previous state, so the tick vanishes entirely.
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 1, bits("0"));
        fold_change(&mut t, ValueKind::Bits, 7, bits("1"));
        fold_change(&mut t, ValueKind::Bits, 7, bits("0"));
        assert_eq!(shape(&t), vec![(1, "0".into())]);
        // A later same-tick callback may then re-establish a change.
        fold_change(&mut t, ValueKind::Bits, 7, bits("1"));
        assert_eq!(shape(&t), vec![(1, "0".into()), (7, "1".into())]);
    }

    #[test]
    fn fold_consecutive_duplicate_suppressed() {
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 1, bits("1"));
        fold_change(&mut t, ValueKind::Bits, 5, bits("1"));
        fold_change(&mut t, ValueKind::Bits, 9, bits("0"));
        assert_eq!(shape(&t), vec![(1, "1".into()), (9, "0".into())]);
    }

    #[test]
    fn fold_first_entry_at_any_tick_kept() {
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 42, bits("x"));
        assert_eq!(shape(&t), vec![(42, "x".into())]);
    }

    #[test]
    fn fold_events_never_collapsed() {
        let mut t = trace();
        fold_change(&mut t, ValueKind::Event, 3, RawValue::Event);
        fold_change(&mut t, ValueKind::Event, 3, RawValue::Event);
        fold_change(&mut t, ValueKind::Event, 4, RawValue::Event);
        assert_eq!(t.times, vec![3, 3, 4]);
    }

    #[test]
    fn fold_real_equality_by_bit_pattern() {
        let mut t = trace();
        fold_change(&mut t, ValueKind::Real, 1, RawValue::Real(f64::NAN));
        fold_change(&mut t, ValueKind::Real, 2, RawValue::Real(f64::NAN));
        assert_eq!(t.times, vec![1], "NaN repeat is a duplicate");
        fold_change(&mut t, ValueKind::Real, 3, RawValue::Real(0.0));
        fold_change(&mut t, ValueKind::Real, 4, RawValue::Real(-0.0));
        assert_eq!(t.times, vec![1, 3, 4], "-0.0 differs from 0.0 by bits");
    }

    #[test]
    fn fold_seed_then_change_at_next_tick() {
        // Windowed WLF shape: carried seed at from-1, real change at from.
        let mut t = trace();
        fold_change(&mut t, ValueKind::Bits, 99, bits("1")); // STARTLOG seed
        fold_change(&mut t, ValueKind::Bits, 100, bits("0")); // event at from
        assert_eq!(shape(&t), vec![(99, "1".into()), (100, "0".into())]);
    }
}
