//! Per-WLF-file backend state and operations.
//!
//! [`WlfBackend`] owns one open `WlfFileId` plus cached metadata + a
//! lazy hierarchy decl cache. The vtable trampolines in [`crate`] are
//! thin wrappers that decode `*mut RwaveSession` to `&mut WlfBackend`
//! and delegate here.

use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};

use super::ERR_PREFIX;
use crate::plugin::builtin::diag::{bridge_err, mentor_diag, to_cstring};
use super::wlf_sys::{
    callback_reason, callback_request, callback_response, libwlf, prop, radix, sel, type_prop,
    WlfFileInfo, WLF_LAST_DELTA,
};
use crate::plugin::ffi::{RwaveEmit, RwaveValueKind, RwaveVarDecl};

/// One owned variable declaration. The CString fields keep the C strings
/// alive across the FFI boundary; we hand out raw pointers into them in
/// [`WlfBackend::var_decls`] and rwave copies what it needs.
struct OwnedVarDecl {
    full_path: CString,
    scope_path: CString,
    type_str: CString,
    width: u32,
    kind: RwaveValueKind,
    /// libwlf archive number, used as the opaque `backend_sid` rwave
    /// trades back into [`WlfBackend::load_traces`] for trace decode.
    backend_sid: u64,
    /// libwlf `WlfSymbolId` pointer. Cached so [`WlfBackend::load_traces`]
    /// can register signal-event callbacks by the same `sid` rwave hands
    /// back, without re-walking the hierarchy.
    ///
    /// Pointer ownership stays with libwlf; we never free it. It is valid
    /// as long as the parent `WlfFileId` is open, which by Drop order is
    /// strictly longer than this Vec.
    symbol_ptr: *mut c_void,
}

pub struct WlfBackend {
    file_id: *mut c_void,
    #[allow(dead_code)] // path is kept for diagnostics; rwave caches it independently
    path: String,

    secs_per_tick: f64,
    timescale_display: CString,
    end_time: i64,
    date_cstr: CString,
    version_cstr: CString,

    decl_cache: Option<Vec<OwnedVarDecl>>,
}

impl WlfBackend {
    pub fn open(path: &str) -> Result<Self, String> {
        let lib = libwlf();

        // Mentor refuses two opens with the same logical name in the
        // same process — see apiparser's notes. Auto-generate a unique
        // one per call.
        static OPEN_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = OPEN_SEQ.fetch_add(1, Ordering::SeqCst);
        let logical = if seq == 0 {
            "main".to_string()
        } else {
            format!("main_{}", seq + 1)
        };

        let path_c = to_cstring(path);
        let logical_c = to_cstring(&logical);

        // SAFETY: pointers are valid NUL-terminated C strings; libwlf
        // returns NULL on failure (no UB on missing licence etc).
        let file_id = unsafe { (lib.wlf_file_open)(path_c.as_ptr(), logical_c.as_ptr()) };
        if file_id.is_null() {
            return Err(bridge_err(ERR_PREFIX, format!("wlfFileOpen returned NULL for {path}")));
        }

        // File-level metadata.
        let mut info = WlfFileInfo::default();
        // SAFETY: file_id is valid; info is a valid out-pointer.
        let rc = unsafe { (lib.wlf_file_info)(file_id, &mut info) };
        if rc != 0 {
            // SAFETY: file_id valid for the diag lookup, even on failure.
            let diag_p = unsafe { (lib.wlf_file_diag)(file_id) };
            let msg = mentor_diag(diag_p, "wlfFileInfo failed");
            // SAFETY: close the file we just opened to avoid the leak.
            unsafe { (lib.wlf_file_close)(file_id) };
            return Err(bridge_err(ERR_PREFIX, format!("wlfFileInfo rc={rc}: {msg}")));
        }

        let mut resolution: c_int = 0;
        // SAFETY: file_id valid; resolution is an out-pointer.
        let _ = unsafe { (lib.wlf_file_resolution)(file_id, &mut resolution) };
        let (secs_per_tick, display) = resolution_to_timescale(resolution);

        let end_time = info.last_time;
        let product = c_buf_to_str(&info.product_name);
        let mti_ver = c_buf_to_str(&info.mti_version);
        let creation_unix = info.creation_time;

        Ok(WlfBackend {
            file_id,
            path: path.to_string(),
            secs_per_tick,
            timescale_display: to_cstring(&display),
            end_time,
            date_cstr: to_cstring(if creation_unix > 0 {
                creation_unix.to_string()
            } else {
                String::new()
            }),
            version_cstr: to_cstring(format!("{product} {mti_ver}").trim()),
            decl_cache: None,
        })
    }

    pub fn timescale(&self) -> (f64, &CStr) {
        (self.secs_per_tick, self.timescale_display.as_c_str())
    }

    pub fn date_cstr(&self) -> &CStr {
        self.date_cstr.as_c_str()
    }

    pub fn version_cstr(&self) -> &CStr {
        self.version_cstr.as_c_str()
    }

    pub fn time_range(&self) -> Option<(i64, i64)> {
        // libwlf reports an explicit start_time only via WlfFileInfo's
        // start_time field, which we don't currently capture (zero for
        // the captures we tested). Treat 0 .. end_time as the range
        // when end_time is positive.
        if self.end_time > 0 {
            Some((0, self.end_time))
        } else {
            None
        }
    }

    pub fn time_step_count(&self) -> usize {
        // libwlf doesn't expose a cheap "distinct timestamps" count.
        // Return 0 so rwave knows it can't trust this value; rwave's
        // commands fall back to length-of-replay computations.
        0
    }

    /// Hand back up to `cap` decls into `buf`, returning the total
    /// hierarchy size on the first call (cap=0). Builds the cache
    /// lazily; subsequent calls reuse it.
    ///
    /// # Safety
    /// `buf` must point to `cap` writable [`RwaveVarDecl`] slots, or be
    /// NULL when `cap == 0`.
    pub unsafe fn var_decls(&mut self, buf: *mut RwaveVarDecl, cap: usize) -> usize {
        if self.decl_cache.is_none() {
            self.build_decl_cache();
        }
        let cache = self.decl_cache.as_ref().expect("decl cache just built");
        let total = cache.len();
        if cap == 0 || buf.is_null() {
            return total;
        }
        let n = total.min(cap);
        for (i, d) in cache.iter().take(n).enumerate() {
            // SAFETY: caller asserts buf has cap slots; n <= cap.
            let dest = unsafe { buf.add(i) };
            unsafe {
                (*dest).full_path = d.full_path.as_ptr();
                (*dest).scope_path = d.scope_path.as_ptr();
                (*dest).type_str = d.type_str.as_ptr();
                (*dest).width = d.width;
                (*dest).kind = d.kind;
                (*dest).backend_sid = d.backend_sid;
            }
        }
        n
    }

    /// Stream trace events back via `emit`. Sets up a libwlf scan over
    /// the file's full time range, registering one IMMEDIATE signal-
    /// event callback per requested sid + a shared time-advance CB.
    /// Each value change → one `emit(ctx, sid, time, buf, len)` invocation.
    ///
    /// Returns 0 on success, nonzero on libwlf-reported failure. The
    /// diagnostic is logged to stderr.
    ///
    /// # Safety
    /// `sids` must point to `n` valid `u64` values, or be NULL when `n == 0`.
    pub fn load_traces(
        &mut self,
        sids: *const u64,
        n: usize,
        emit: RwaveEmit,
        ctx: *mut c_void,
    ) -> c_int {
        if sids.is_null() || n == 0 {
            return 0;
        }
        // SAFETY: caller asserts n valid u64 values at sids.
        let sid_slice: &[u64] = unsafe { std::slice::from_raw_parts(sids, n) };
        let end = self.resolve_end_time();
        match self.run_scan(sid_slice, 0, end, emit, ctx) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("rwave-wlf: {e}");
                1
            }
        }
    }

    /// Windowed decode: for each signal, the value in effect entering the
    /// window followed by every change in the window, via `emit` in time
    /// order. `to == i64::MAX` means "to the end".
    ///
    /// libwlf makes this a seek, not a replay: `wlfReadDataOverRange` with a
    /// mid-file start walks only the window's time steps (measured on Questa
    /// 10.7c: a 200 ns window anywhere in a 20 M-step capture costs ~1–2 ms
    /// vs ~600 ms for the full scan), and at scan start every registered
    /// signal fires a `STARTLOG` callback carrying its value entering the
    /// window — the seed comes free. Signals whose logging starts after the
    /// window fire nothing at all and stay absent, exactly matching the
    /// windowed contract.
    ///
    /// The one deviation from the FST/FSDB backends: libwlf does not report
    /// *when* the carried value was last changed (its reverse-search API is
    /// non-functional outside a live vsim session), so the seed is tagged at
    /// `from - 1` — see [`run_scan`] for why `- 1` — rather than the change's
    /// true tick. Point/window consumers read the seed's value, not its time.
    ///
    /// # Safety
    /// Same contract as [`load_traces`](Self::load_traces).
    pub fn load_traces_windowed(
        &mut self,
        sids: *const u64,
        n: usize,
        from: i64,
        to: i64,
        emit: RwaveEmit,
        ctx: *mut c_void,
    ) -> c_int {
        if sids.is_null() || n == 0 {
            return 0;
        }
        // SAFETY: caller asserts n valid u64 values at sids.
        let sid_slice: &[u64] = unsafe { std::slice::from_raw_parts(sids, n) };
        let (from, to) = clamp_window(from, to, self.resolve_end_time());
        match self.run_scan(sid_slice, from, to, emit, ctx) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("rwave-wlf: {e}");
                1
            }
        }
    }

    // ---- internals -------------------------------------------------------

    /// End tick for scans: the value captured at open, else `wlfFileEndTime`
    /// (some writers leave `WlfFileInfo.lastTime` zero).
    fn resolve_end_time(&self) -> i64 {
        if self.end_time > 0 {
            return self.end_time;
        }
        let mut t: i64 = 0;
        // SAFETY: file_id valid; t is a writable i64.
        let _ = unsafe { (libwlf().wlf_file_end_time)(self.file_id, &mut t) };
        t
    }

    fn build_decl_cache(&mut self) {
        let lib = libwlf();
        // SAFETY: file_id valid for any wlfFile* call.
        let top = unsafe { (lib.wlf_file_get_top_region)(self.file_id) };
        let mut out: Vec<OwnedVarDecl> = Vec::new();
        if !top.is_null() {
            walk(top, "", &mut out);
        }
        self.decl_cache = Some(out);
    }

    /// Run a libwlf scan over `[from, to]` for the requested `sids`. One
    /// IMMEDIATE signal-event callback per sid; a single time-advance
    /// callback updates `cur_time` so each signal callback can tag its
    /// emit with the right tick.
    ///
    /// Emission rules (see [`signal_event_cb`]): only `EVENT` and
    /// `STARTLOG` reasons produce values; `ENDLOG` (a fake trailing
    /// change libwlf fires at scan end with the then-current value) and
    /// everything else are dropped. Callbacks arriving before the first
    /// time-advance CB belong to the scan start: a real change exactly at
    /// `from` (EVENT) is tagged `from`, while a carried value (STARTLOG)
    /// is tagged `from - 1` so window consumers — which treat trace
    /// entries at `>= from` as in-window changes — never mistake the
    /// carried state for an event at `from`. A full scan starts at 0, so
    /// there its STARTLOG keeps today's tag of 0 (the initial value _is_
    /// the t=0 state, as in a VCD `$dumpvars` block).
    ///
    /// Memory model: the [`SharedScanCtx`] and one [`PerSignalScanData`]
    /// per signal are heap-allocated and their addresses handed to
    /// libwlf as `cbData`. We keep them in Vecs / Boxes for the full
    /// duration of the scan so the addresses stay valid. After the scan
    /// returns, the value ids are destroyed, the pack is destroyed, and
    /// the Boxes drop normally.
    fn run_scan(
        &mut self,
        sids: &[u64],
        from: i64,
        to: i64,
        emit: RwaveEmit,
        rwave_ctx: *mut c_void,
    ) -> Result<(), String> {
        if self.decl_cache.is_none() {
            self.build_decl_cache();
        }
        let cache = self.decl_cache.as_ref().expect("decl cache built");

        // sid → (symbol_ptr, kind). Built once; small enough for plain HashMap.
        let mut sid_info: HashMap<u64, (*mut c_void, RwaveValueKind)> =
            HashMap::with_capacity(cache.len());
        for d in cache {
            if !d.symbol_ptr.is_null() {
                sid_info.insert(d.backend_sid, (d.symbol_ptr, d.kind));
            }
        }

        let lib = libwlf();

        // SAFETY: wlfPackCreate has no preconditions; NULL on failure.
        let pack = unsafe { (lib.wlf_pack_create)() };
        if pack.is_null() {
            return Err(bridge_err(ERR_PREFIX, "wlfPackCreate returned NULL"));
        }

        // Shared scan state. Heap-allocated so its address is stable
        // for the duration of the scan; we pass `&mut *shared` as raw
        // pointer in two places (time CB cb_data, and as the back-pointer
        // inside each PerSignalScanData).
        let mut shared = Box::new(SharedScanCtx {
            cur_time: from,
            cur_delta: 0,
            seed_tag: seed_tag_for(from),
            have_time: false,
            rwave_emit: emit,
            rwave_ctx,
        });
        let shared_ptr: *mut SharedScanCtx = &mut *shared;

        // Per-signal scan data. Box so each has a stable address.
        let mut sig_datas: Vec<Box<PerSignalScanData>> = Vec::with_capacity(sids.len());

        let scan_result = (|| -> Result<(), String> {
            for &sid in sids {
                let Some(&(sym, kind)) = sid_info.get(&sid) else {
                    // Unknown sid → rwave gets an empty trace for it; not an error.
                    continue;
                };

                // SAFETY: sym was obtained from the hierarchy walk and is
                // still owned by libwlf (file not closed yet).
                let val_id = unsafe { (lib.wlf_value_create)(sym) };
                if val_id.is_null() {
                    return Err(bridge_err(ERR_PREFIX, format!(
                        "wlfValueCreate returned NULL for sid {sid}"
                    )));
                }

                let radix_val = match kind {
                    RwaveValueKind::Bits => radix::BINARY,
                    _ => radix::DEFAULT,
                };

                let mut data = Box::new(PerSignalScanData {
                    backend_sid: sid,
                    value_id: val_id,
                    radix: radix_val,
                    shared: shared_ptr,
                });
                let data_ptr: *mut PerSignalScanData = &mut *data;

                let mut cb_ptr: *mut c_void = std::ptr::null_mut();
                // SAFETY: all pointer args valid; signal_event_cb has C ABI.
                let rc = unsafe {
                    (lib.wlf_append_signal_event_cb)(
                        pack,
                        sym,
                        val_id,
                        callback_request::IMMEDIATE,
                        signal_event_cb,
                        data_ptr as *mut c_void,
                        &mut cb_ptr,
                    )
                };
                if rc != 0 {
                    // SAFETY: val_id was created by wlfValueCreate.
                    unsafe { (lib.wlf_value_destroy)(val_id) };
                    return Err(bridge_err(ERR_PREFIX, format!(
                        "wlfAppendSignalEventCB rc={rc} for sid {sid}"
                    )));
                }
                sig_datas.push(data);
            }

            if sig_datas.is_empty() {
                // No matching sids → nothing to scan; libwlf would
                // skip the scan anyway without IMMEDIATE CBs.
                return Ok(());
            }

            // endDelta = WLF_LAST_DELTA so files logged with -nowlfcollapse
            // keep their end-tick delta events (0 and LAST_DELTA are
            // identical on default delta-collapsed captures).
            // SAFETY: pack/time_advance_cb/shared_ptr all valid; delta CB
            // slot is declared as opaque void* so passing NULL is allowed.
            let rc = unsafe {
                (lib.wlf_read_data_over_range)(
                    pack,
                    from,
                    0,
                    to,
                    WLF_LAST_DELTA,
                    time_advance_cb,
                    std::ptr::null_mut(),
                    shared_ptr as *mut c_void,
                )
            };
            if rc != 0 {
                // SAFETY: file_id valid; diag returns a libwlf-owned string.
                let diag_p = unsafe { (lib.wlf_file_diag)(self.file_id) };
                let msg = mentor_diag(diag_p, "wlfReadDataOverRange failed");
                return Err(bridge_err(ERR_PREFIX, format!(
                    "wlfReadDataOverRange rc={rc}: {msg}"
                )));
            }
            Ok(())
        })();

        // Cleanup: destroy each WlfValueId, then the pack. Boxes drop
        // automatically with the function frame.
        for data in &sig_datas {
            // SAFETY: value_id created via wlfValueCreate above.
            unsafe { (lib.wlf_value_destroy)(data.value_id) };
        }
        // SAFETY: pack created via wlfPackCreate above.
        unsafe { (lib.wlf_pack_destroy)(pack) };

        scan_result
    }
}

impl Drop for WlfBackend {
    fn drop(&mut self) {
        if !self.file_id.is_null() {
            // SAFETY: file_id was acquired via wlfFileOpen; close once.
            unsafe { (libwlf().wlf_file_close)(self.file_id) };
            self.file_id = std::ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// Scan callback state and C-ABI trampolines
// ---------------------------------------------------------------------------

/// State shared between the time-advance CB (which writes `cur_time`)
/// and every per-signal CB (which reads it). Heap-allocated by
/// [`WlfBackend::run_scan`]; addresses handed to libwlf as `cbData`.
#[repr(C)]
struct SharedScanCtx {
    cur_time: i64,
    #[allow(dead_code)] // delta is tracked for future delta-mode support
    cur_delta: c_int,
    /// Tick to stamp on carried-value STARTLOG callbacks that arrive before
    /// the first time-advance CB: `from - 1` for a windowed scan (so the
    /// seed sorts before, and is never confused with, a change at `from`),
    /// 0 for a full scan.
    seed_tag: i64,
    /// Set by the first time-advance CB. Until then the scan is delivering
    /// its start-of-window state (STARTLOG stuffing + changes exactly at
    /// `from`).
    have_time: bool,
    rwave_emit: RwaveEmit,
    rwave_ctx: *mut c_void,
}

/// Per-signal scan state. One per registered IMMEDIATE callback; passed
/// to libwlf as the signal CB's `cbData`. Holds enough to read the
/// current value (via `value_id` + `radix`) and emit it tagged with the
/// shared `cur_time`.
#[repr(C)]
struct PerSignalScanData {
    backend_sid: u64,
    value_id: *mut c_void,
    radix: c_int,
    shared: *mut SharedScanCtx,
}

/// libwlf time-advance callback. Updates `cur_time` / `cur_delta` on
/// the shared state. cb_data is a `*mut SharedScanCtx`.
unsafe extern "C" fn time_advance_cb(
    cb_data: *mut c_void,
    new_time: i64,
    new_delta: c_int,
) -> c_int {
    if cb_data.is_null() {
        return callback_response::CONTINUE;
    }
    let shared = cb_data as *mut SharedScanCtx;
    // Field writes via raw pointer; libwlf is single-threaded within
    // a scan, no aliasing race with the signal CB which only reads.
    unsafe {
        (*shared).cur_time = new_time;
        (*shared).cur_delta = new_delta;
        (*shared).have_time = true;
    }
    callback_response::CONTINUE
}

/// libwlf signal-event callback. Pulls the current value via
/// `wlfValueToString`, tags it per the reason (see [`WlfBackend::run_scan`]
/// for the tagging rules), and hands it back to rwave via the cached
/// `RwaveEmit`. Reasons other than EVENT / STARTLOG carry no change —
/// ENDLOG repeats the current value at scan end, DNE has no value at all —
/// and are dropped. cb_data is a `*mut PerSignalScanData`.
unsafe extern "C" fn signal_event_cb(cb_data: *mut c_void, reason: c_int) -> c_int {
    if cb_data.is_null() {
        return callback_response::CONTINUE;
    }
    let data = cb_data as *const PerSignalScanData;
    // SAFETY: data and shared addresses live for the scan; libwlf
    // is single-threaded so no concurrent &mut elsewhere.
    let shared = unsafe { (*data).shared };
    if shared.is_null() {
        return callback_response::CONTINUE;
    }

    let cur_time = unsafe { (*shared).cur_time };
    let tag = match reason {
        callback_reason::EVENT => cur_time,
        callback_reason::STARTLOG => {
            if unsafe { (*shared).have_time } {
                cur_time
            } else {
                unsafe { (*shared).seed_tag }
            }
        }
        _ => return callback_response::CONTINUE,
    };

    let value_id = unsafe { (*data).value_id };
    let radix_val = unsafe { (*data).radix };
    let sid = unsafe { (*data).backend_sid };
    let emit = unsafe { (*shared).rwave_emit };
    let rwave_ctx = unsafe { (*shared).rwave_ctx };

    // SAFETY: value_id valid for the scan; wlfValueToString returns a
    // pointer into pack-internal memory valid until next pack operation
    // (which is the next callback fire, so it's stable for this emit call).
    let buf_ptr = unsafe { (libwlf().wlf_value_to_string)(value_id, radix_val, 0) };
    let (buf, len) = if buf_ptr.is_null() {
        (std::ptr::null(), 0u32)
    } else {
        // SAFETY: buf_ptr is NUL-terminated per Mentor's docs.
        let len = unsafe { CStr::from_ptr(buf_ptr) }.to_bytes().len() as u32;
        (buf_ptr, len)
    };

    // SAFETY: emit is a valid C ABI function pointer rwave handed us.
    unsafe { emit(rwave_ctx, sid, tag, buf, len) };
    callback_response::CONTINUE
}

// ---------------------------------------------------------------------------
// Hierarchy walk (recursive)
// ---------------------------------------------------------------------------

fn walk(scope: *mut c_void, parent_path: &str, out: &mut Vec<OwnedVarDecl>) {
    let lib = libwlf();

    // SAFETY: scope is a non-NULL WlfSymbolId from libwlf; mask all-1s
    // returns every child.
    let iter = unsafe { (lib.wlf_sym_children)(scope, c_uint::MAX) };
    let children = drain_iter(iter);

    for child in children {
        if child.is_null() {
            continue;
        }
        // SAFETY: child is a non-NULL WlfSymbolId from the iterator.
        let name_p = unsafe { (lib.wlf_sym_prop_string)(child, prop::SYMBOL_NAME) };
        let name = if name_p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name_p) }.to_string_lossy().into_owned()
        };

        let sub_path = if parent_path.is_empty() {
            name.clone()
        } else if name.is_empty() {
            parent_path.to_string()
        } else {
            format!("{parent_path}.{name}")
        };

        // SAFETY: child valid; SYMBOL_TYPE returns u32 sel mask.
        let sel_mask = unsafe { (lib.wlf_sym_prop_symbol_sel)(child, prop::SYMBOL_TYPE) };

        if sel::is_scope(sel_mask) {
            walk(child, &sub_path, out);
        } else {
            out.push(leaf_to_decl(child, sub_path, parent_path.to_string(), sel_mask));
        }
    }
}

fn leaf_to_decl(
    sym: *mut c_void,
    full_path: String,
    scope_path: String,
    sel_mask: u32,
) -> OwnedVarDecl {
    let lib = libwlf();
    // SAFETY: sym valid; ARCHIVE_NUMBER returns int.
    let archive = unsafe { (lib.wlf_sym_prop_int)(sym, prop::ARCHIVE_NUMBER) };
    let width = signal_width(sym);
    let kind = classify_kind(sel_mask);
    let type_str = classify_type(sel_mask);

    OwnedVarDecl {
        full_path: to_cstring(&full_path),
        scope_path: to_cstring(&scope_path),
        type_str: to_cstring(type_str),
        width,
        kind,
        // Negative archive numbers are sentinels in some captures; we
        // clamp to 0 so the sid stays a positive u64.
        backend_sid: archive.max(0) as u64,
        symbol_ptr: sym,
    }
}

fn signal_width(sym: *mut c_void) -> u32 {
    let lib = libwlf();
    // SAFETY: sym valid; TYPE_ID returns a type-id pointer or NULL.
    let type_id = unsafe { (lib.wlf_sym_prop_type_id)(sym, prop::TYPE_ID) };
    if type_id.is_null() {
        return 1;
    }
    for p in [
        type_prop::REGISTER_WIDTH,
        type_prop::ARRAY_LENGTH,
        type_prop::VALUE_SIZE,
    ] {
        // SAFETY: type_id valid; the property selectors are integer-returning.
        let w = unsafe { (lib.wlf_type_prop_int)(type_id, p) };
        if w > 0 {
            return w as u32;
        }
    }
    1
}

/// Map the sel mask to one of the four [`RwaveValueKind`] variants. The
/// dominant case (logic vectors) is `Bits`; real / event are the two
/// non-bits special cases.
fn classify_kind(sel_mask: u32) -> RwaveValueKind {
    if (sel_mask & sel::REAL) != 0 {
        RwaveValueKind::Real
    } else if (sel_mask & sel::NAMED_EVENT) != 0 {
        RwaveValueKind::Event
    } else {
        RwaveValueKind::Bits
    }
}

/// Map the sel mask to one of rwave's known type_str values. Unknowns
/// default to "wire".
fn classify_type(sel_mask: u32) -> &'static str {
    // Selected mask bits from wlf_api.h, all signal-category bits.
    const REG: u32 = 0x0080_0000;
    const NET: u32 = 0x0020_0000;
    const SIGNAL_VHDL: u32 = 0x0000_0100;
    const VARIABLE_VHDL: u32 = 0x0000_0200;
    const CONSTANT: u32 = 0x0000_0400;
    const INTEGER: u32 = 0x0100_0000;
    const TIME: u32 = 0x0200_0000;

    if (sel_mask & sel::REAL) != 0 {
        "real"
    } else if (sel_mask & sel::NAMED_EVENT) != 0 {
        "event"
    } else if (sel_mask & REG) != 0 {
        "reg"
    } else if (sel_mask & NET) != 0 {
        "wire"
    } else if (sel_mask & INTEGER) != 0 {
        "integer"
    } else if (sel_mask & TIME) != 0 {
        "time"
    } else if (sel_mask & CONSTANT) != 0 {
        "parameter"
    } else if (sel_mask & (SIGNAL_VHDL | VARIABLE_VHDL)) != 0 {
        "wire"
    } else {
        "wire"
    }
}

/// Drain a `WlfIterId` into a plain Vec, then destroy the iterator.
/// apiparser learned the hard way that overlapping iterators on the
/// same scope hang libwlf — we always materialize one iterator's
/// contents before starting another.
fn drain_iter(iter: *mut c_void) -> Vec<*mut c_void> {
    if iter.is_null() {
        return Vec::new();
    }
    let lib = libwlf();
    let mut ids = Vec::new();
    loop {
        // SAFETY: iter valid; wlfIterate returns NULL on end-of-iterator.
        let sym = unsafe { (lib.wlf_iterate)(iter) };
        if sym.is_null() {
            break;
        }
        ids.push(sym);
    }
    // SAFETY: iter valid; we own it and have drained.
    unsafe { (lib.wlf_iterator_destroy)(iter) };
    ids
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Clamp a windowed request onto the file's `[0, end]` axis: a `from` beyond
/// the end anchors at the final instant (the stuffed values there answer any
/// later query); `to` at or beyond the end — including the i64::MAX sentinel —
/// becomes the end itself; a degenerate `to < from` must not shrink the scan
/// below its start.
fn clamp_window(from: i64, to: i64, end: i64) -> (i64, i64) {
    let from = from.clamp(0, end);
    (from, to.min(end).max(from))
}

/// Tick stamped on carried-value STARTLOG callbacks at scan start: one before
/// the window so consumers never read the carried state as a change at `from`
/// (a full scan starts at 0, where the initial value genuinely is the t=0
/// state).
fn seed_tag_for(from: i64) -> i64 {
    if from > 0 { from - 1 } else { 0 }
}

fn resolution_to_timescale(res: c_int) -> (f64, String) {
    // res values 0..=17 in wlf_api.h map to 10^(res - 15) seconds/tick.
    let exp: i32 = (res as i32) - 15;
    let secs = 10f64.powi(exp);
    let display = match res {
        0 => "1fs",
        1 => "10fs",
        2 => "100fs",
        3 => "1ps",
        4 => "10ps",
        5 => "100ps",
        6 => "1ns",
        7 => "10ns",
        8 => "100ns",
        9 => "1us",
        10 => "10us",
        11 => "100us",
        12 => "1ms",
        13 => "10ms",
        14 => "100ms",
        15 => "1s",
        16 => "10s",
        17 => "100s",
        _ => "1ns",
    }
    .to_string();
    (secs, display)
}

fn c_buf_to_str(buf: &[u8]) -> String {
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).trim().to_string()
}

// We don't expose anything from this module beyond `WlfBackend` itself.
// Suppress the lint warning that fires when private items in a binary
// crate look unused from outside.
#[allow(dead_code)]
fn _private_marker() {}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::wlf_sys::sel;

    #[test]
    fn resolution_zero_is_1fs() {
        let (secs, display) = resolution_to_timescale(0);
        assert_eq!(display, "1fs");
        assert!((secs - 1e-15).abs() < 1e-30);
    }

    #[test]
    fn resolution_six_is_1ns() {
        let (secs, display) = resolution_to_timescale(6);
        assert_eq!(display, "1ns");
        assert!((secs - 1e-9).abs() < 1e-20);
    }

    #[test]
    fn resolution_nine_is_1us() {
        let (_secs, display) = resolution_to_timescale(9);
        assert_eq!(display, "1us");
    }

    #[test]
    fn resolution_fifteen_is_1s() {
        let (secs, display) = resolution_to_timescale(15);
        assert_eq!(display, "1s");
        assert!((secs - 1.0).abs() < 1e-12);
    }

    #[test]
    fn resolution_out_of_range_falls_back() {
        // Anything outside 0..=17 falls back to the safe "1ns" display.
        let (_, display) = resolution_to_timescale(99);
        assert_eq!(display, "1ns");
    }

    #[test]
    fn clamp_window_passes_through_in_range() {
        assert_eq!(clamp_window(100, 200, 1000), (100, 200));
    }

    #[test]
    fn clamp_window_unbounded_sentinel_becomes_end() {
        assert_eq!(clamp_window(100, i64::MAX, 1000), (100, 1000));
    }

    #[test]
    fn clamp_window_from_beyond_end_anchors_at_end() {
        assert_eq!(clamp_window(5000, i64::MAX, 1000), (1000, 1000));
    }

    #[test]
    fn clamp_window_negative_from_floors_at_zero() {
        assert_eq!(clamp_window(-5, 200, 1000), (0, 200));
    }

    #[test]
    fn clamp_window_degenerate_to_below_from() {
        assert_eq!(clamp_window(300, 200, 1000), (300, 300));
    }

    #[test]
    fn seed_tag_precedes_window_start() {
        assert_eq!(seed_tag_for(100), 99);
        assert_eq!(seed_tag_for(1), 0);
    }

    #[test]
    fn seed_tag_full_scan_keeps_zero() {
        assert_eq!(seed_tag_for(0), 0);
    }

    #[test]
    fn c_buf_to_str_strips_nul_padding() {
        let buf: Vec<u8> = b"hello\0\0\0\0\0\0".to_vec();
        assert_eq!(c_buf_to_str(&buf), "hello");
    }

    #[test]
    fn c_buf_to_str_trims_whitespace() {
        let buf: Vec<u8> = b"  spaced  \0\0\0".to_vec();
        assert_eq!(c_buf_to_str(&buf), "spaced");
    }

    #[test]
    fn c_buf_to_str_empty_when_only_nul() {
        let buf: Vec<u8> = b"\0\0\0".to_vec();
        assert_eq!(c_buf_to_str(&buf), "");
    }

    #[test]
    fn c_buf_to_str_handles_no_nul_terminator() {
        let buf: Vec<u8> = b"raw".to_vec();
        assert_eq!(c_buf_to_str(&buf), "raw");
    }

    #[test]
    fn classify_kind_real_bit_is_real() {
        assert_eq!(classify_kind(sel::REAL), RwaveValueKind::Real);
    }

    #[test]
    fn classify_kind_named_event_bit_is_event() {
        assert_eq!(classify_kind(sel::NAMED_EVENT), RwaveValueKind::Event);
    }

    #[test]
    fn classify_kind_default_is_bits() {
        // Plain "signal" sel mask with no real / event bits.
        assert_eq!(classify_kind(0x0000_0100), RwaveValueKind::Bits);
        assert_eq!(classify_kind(0), RwaveValueKind::Bits);
    }

    #[test]
    fn classify_type_real_string() {
        assert_eq!(classify_type(sel::REAL), "real");
    }

    #[test]
    fn classify_type_event_string() {
        assert_eq!(classify_type(sel::NAMED_EVENT), "event");
    }

    #[test]
    fn classify_type_reg_string() {
        // wlfSelReg = 0x0080_0000
        assert_eq!(classify_type(0x0080_0000), "reg");
    }

    #[test]
    fn classify_type_net_string() {
        // wlfSelNet = 0x0020_0000
        assert_eq!(classify_type(0x0020_0000), "wire");
    }

    #[test]
    fn classify_type_unknown_falls_back_to_wire() {
        assert_eq!(classify_type(0), "wire");
    }
}
