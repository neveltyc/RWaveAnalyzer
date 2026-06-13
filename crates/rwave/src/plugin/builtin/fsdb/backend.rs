//! Per-FSDB-file backend state, driven through Synopsys NPI (`npi_fsdb_*`).

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use super::diag::{bridge_err, to_cstring};
use super::fsdb_sys::{
    file_prop, npi, sig_prop, val_fmt, LibNpi, NpiFsdbValue, NpiHandle,
};
use crate::plugin::ffi::{RwaveEmit, RwaveValueKind, RwaveVarDecl};

struct OwnedVarDecl {
    full_path:  CString,
    scope_path: CString,
    type_str:   CString,
    width:      u32,
    kind:       RwaveValueKind,
    /// The NPI `npiFsdbSigHandle` (a `void*`) as an integer. Valid until
    /// `npi_fsdb_close`; reused directly by `create_vct` in load_traces.
    backend_sid: u64,
}

pub struct FsdbBackend {
    /// `npiFsdbFileHandle`.
    session: NpiHandle,
    #[allow(dead_code)]
    path: String,

    secs_per_tick:     f64,
    timescale_display: CString,
    date_cstr:         CString,
    version_cstr:      CString,
    time_lo:           i64,
    time_hi:           i64,
    time_range_ok:     bool,

    decl_cache: Option<Vec<OwnedVarDecl>>,
}

impl FsdbBackend {
    pub fn open(path: &str) -> Result<Self, String> {
        let n = npi();
        let path_c = to_cstring(path);

        let session = unsafe { (n.fsdb_open)(path_c.as_ptr()) };
        if session.is_null() {
            return Err(bridge_err(format!(
                "npi_fsdb_open returned NULL for {path} \
                 (check the Verdi-Ultra license, RWAVE_FSDB_LIB, and that \
                 the Verdi environment is sourced)"
            )));
        }

        let version = read_cstr(unsafe { (n.file_property_str)(file_prop::VERSION, session) });
        let date    = read_cstr(unsafe { (n.file_property_str)(file_prop::SIM_DATE, session) });
        let scale   = read_cstr(unsafe { (n.file_property_str)(file_prop::SCALE_UNIT, session) });
        let (secs, display) = parse_scale_unit(&scale);

        let mut lo: u64 = 0;
        let mut hi: u64 = 0;
        let ok = unsafe { (n.min_time)(session, &mut lo) } != 0
            && unsafe { (n.max_time)(session, &mut hi) } != 0;

        Ok(FsdbBackend {
            session,
            path: path.to_string(),
            secs_per_tick:     secs,
            timescale_display: to_cstring(display),
            date_cstr:         to_cstring(date),
            version_cstr:      to_cstring(version),
            time_lo:           lo as i64,
            time_hi:           hi as i64,
            time_range_ok:     ok,
            decl_cache:        None,
        })
    }

    pub fn timescale(&self) -> (f64, &CStr) { (self.secs_per_tick, self.timescale_display.as_c_str()) }
    pub fn date_cstr(&self) -> &CStr        { self.date_cstr.as_c_str() }
    pub fn version_cstr(&self) -> &CStr     { self.version_cstr.as_c_str() }

    pub fn time_range(&self) -> Option<(i64, i64)> {
        if self.time_range_ok { Some((self.time_lo, self.time_hi)) } else { None }
    }

    /// NPI has no O(1) total-event count; 0 lets rwave fall back to replay length.
    pub fn time_step_count(&self) -> usize { 0 }

    /// # Safety
    /// `buf` must point to `cap` writable `RwaveVarDecl` slots, or be NULL
    /// when `cap == 0`.
    pub unsafe fn var_decls(&mut self, buf: *mut RwaveVarDecl, cap: usize) -> usize {
        if self.decl_cache.is_none() {
            self.build_decl_cache();
        }
        let cache = self.decl_cache.as_ref().expect("decl cache built");
        let total = cache.len();
        if cap == 0 || buf.is_null() {
            return total;
        }
        let n = total.min(cap);
        for (i, d) in cache.iter().take(n).enumerate() {
            let dest = unsafe { buf.add(i) };
            unsafe {
                (*dest).full_path   = d.full_path.as_ptr();
                (*dest).scope_path  = d.scope_path.as_ptr();
                (*dest).type_str    = d.type_str.as_ptr();
                (*dest).width       = d.width;
                (*dest).kind        = d.kind;
                (*dest).backend_sid = d.backend_sid;
            }
        }
        n
    }

    /// # Safety
    /// `sids` must point to `n` valid `u64`s (each a `backend_sid` we handed
    /// out in `var_decls`), or be NULL when `n == 0`.
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
        // We need per-signal `kind` to pick the value format; ensure the
        // cache exists and index it by backend_sid.
        if self.decl_cache.is_none() {
            self.build_decl_cache();
        }
        let lib = npi();
        let sid_slice: &[u64] = unsafe { std::slice::from_raw_parts(sids, n) };
        for &sid in sid_slice {
            let is_real = self
                .decl_cache
                .as_ref()
                .and_then(|c| c.iter().find(|d| d.backend_sid == sid))
                .map(|d| d.kind == RwaveValueKind::Real)
                .unwrap_or(false);
            self.stream_signal(lib, sid, is_real, emit, ctx);
        }
        0
    }

    /// Stream every value change of one signal in time order via `emit`.
    fn stream_signal(&self, lib: &LibNpi, sid: u64, is_real: bool, emit: RwaveEmit, ctx: *mut c_void) {
        let sig: NpiHandle = sid as usize as NpiHandle;
        let vct = unsafe { (lib.create_vct)(sig) };
        if vct.is_null() {
            return;
        }
        let fmt = if is_real { val_fmt::REAL } else { val_fmt::BIN_STR };
        // Documented idiom (NPI ref, "General Flow"): goto_first to
        // initialise, read the current VC, then advance with goto_next until
        // it returns 0. goto_first/goto_next are non-zero while positioned.
        if unsafe { (lib.goto_first)(vct) } != 0 {
            loop {
                let mut tick: u64 = 0;
                unsafe { (lib.vct_time)(vct, &mut tick) };
                let mut v = NpiFsdbValue::with_format(fmt);
                if unsafe { (lib.vct_value)(vct, &mut v) } != 0 {
                    if is_real {
                        let r = unsafe { v.value.real };
                        if let Ok(cs) = CString::new(format!("{r}")) {
                            let len = cs.as_bytes().len() as u32;
                            unsafe { emit(ctx, sid, tick as i64, cs.as_ptr(), len) };
                        }
                    } else {
                        let s = unsafe { v.value.str_ };
                        if !s.is_null() {
                            let len = unsafe { CStr::from_ptr(s) }.to_bytes().len() as u32;
                            unsafe { emit(ctx, sid, tick as i64, s, len) };
                        }
                    }
                }
                if unsafe { (lib.goto_next)(vct) } == 0 {
                    break;
                }
            }
        }
        unsafe { (lib.release_vct)(vct) };
    }

    fn build_decl_cache(&mut self) {
        let lib = npi();
        let mut out: Vec<OwnedVarDecl> = Vec::new();

        // Walk every scope; signals carry their own full hierarchical name,
        // so we only iterate scopes to enumerate the signals under them.
        let top = unsafe { (lib.iter_top_scope)(self.session) };
        if !top.is_null() {
            loop {
                let scope = unsafe { (lib.iter_scope_next)(top) };
                if scope.is_null() {
                    break;
                }
                collect_scope(lib, scope, &mut out);
            }
            let _ = unsafe { (lib.iter_scope_stop)(top) };
        }
        self.decl_cache = Some(out);
    }
}

/// Recursively collect leaf signals under `scope` (signals here + child scopes).
fn collect_scope(lib: &LibNpi, scope: NpiHandle, out: &mut Vec<OwnedVarDecl>) {
    // signals directly in this scope
    let sit = unsafe { (lib.iter_sig)(scope) };
    if !sit.is_null() {
        loop {
            let sig = unsafe { (lib.iter_sig_next)(sit) };
            if sig.is_null() {
                break;
            }
            record_sig(lib, sig, out);
        }
        let _ = unsafe { (lib.iter_sig_stop)(sit) };
    }
    // child scopes
    let cit = unsafe { (lib.iter_child_scope)(scope) };
    if !cit.is_null() {
        loop {
            let child = unsafe { (lib.iter_scope_next)(cit) };
            if child.is_null() {
                break;
            }
            collect_scope(lib, child, out);
        }
        let _ = unsafe { (lib.iter_scope_stop)(cit) };
    }
}

fn record_sig(lib: &LibNpi, sig: NpiHandle, out: &mut Vec<OwnedVarDecl>) {
    let full = read_cstr(unsafe { (lib.sig_property_str)(sig_prop::FULL_NAME, sig) });
    if full.is_empty() {
        return;
    }
    let mut range_size: c_int = 0;
    unsafe { (lib.sig_property)(sig_prop::RANGE_SIZE, sig, &mut range_size) };
    let mut is_real: c_int = 0;
    unsafe { (lib.sig_property)(sig_prop::IS_REAL, sig, &mut is_real) };
    let mut is_string: c_int = 0;
    unsafe { (lib.sig_property)(sig_prop::IS_STRING, sig, &mut is_string) };
    let mut has_member: c_int = 0;
    unsafe { (lib.sig_property)(sig_prop::HAS_MEMBER, sig, &mut has_member) };

    // Composite signals (struct/union/multi-dim array — NPI doc's
    // npiFsdbSigHasMember) read back as a braced aggregate "{elem,elem,…}",
    // and RANGE_SIZE is then the element count, not a bit width. Surface them
    // as an opaque Str so they aren't mislabelled a flat bit vector
    // (per-member expansion via iter_member is a possible later refinement).
    // Width follows rwave's ABI: real bit width for a plain vector, 1 for
    // real / string / composite.
    let (kind, type_str, width) = if is_real != 0 {
        (RwaveValueKind::Real, "real", 1)
    } else if has_member != 0 {
        (RwaveValueKind::Str, "array", 1)
    } else if is_string != 0 {
        (RwaveValueKind::Str, "string", 1)
    } else {
        (RwaveValueKind::Bits, "wire", (range_size.max(0) as u32).max(1))
    };
    let (scope, _leaf) = split_scope_leaf(&full);
    out.push(OwnedVarDecl {
        full_path:   to_cstring(&full),
        scope_path:  to_cstring(scope),
        type_str:    to_cstring(type_str),
        width,
        kind,
        backend_sid: sig as usize as u64,
    });
}

impl Drop for FsdbBackend {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe { (npi().fsdb_close)(self.session) };
            self.session = std::ptr::null_mut();
        }
    }
}

fn read_cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Split on the rightmost '.' or '/' (FSDB hierarchies use either separator).
fn split_scope_leaf(full: &str) -> (&str, &str) {
    let dot   = full.rfind('.');
    let slash = full.rfind('/');
    let split = match (dot, slash) {
        (Some(d), Some(s)) => Some(d.max(s)),
        (Some(d), None)    => Some(d),
        (None, Some(s))    => Some(s),
        (None, None)       => None,
    };
    match split {
        Some(i) => (&full[..i], &full[i + 1..]),
        None    => ("", full),
    }
}

/// "1ns" / "100ps" → (seconds, display). Unknown or empty falls back to "1ns".
fn parse_scale_unit(s: &str) -> (f64, String) {
    let t = s.trim();
    if t.is_empty() {
        return (1e-9, "1ns".to_string());
    }
    let split = t
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(t.len());
    let (digits, unit) = t.split_at(split);
    let mag: f64 = digits.parse().unwrap_or(1.0);
    let unit_norm = unit.trim().to_lowercase();
    let scale = match unit_norm.as_str() {
        "fs" => 1e-15,
        "ps" => 1e-12,
        "ns" => 1e-9,
        "us" | "µs" => 1e-6,
        "ms" => 1e-3,
        "s"  => 1.0,
        _    => return (1e-9, "1ns".to_string()),
    };
    let display = format!("{}{}", digits, unit_norm);
    (mag * scale, display)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scale_unit_1ns() {
        let (secs, disp) = parse_scale_unit("1ns");
        assert!((secs - 1e-9).abs() < 1e-20);
        assert_eq!(disp, "1ns");
    }

    #[test]
    fn parse_scale_unit_100ps() {
        let (secs, disp) = parse_scale_unit("100ps");
        assert!((secs - 100e-12).abs() < 1e-20);
        assert_eq!(disp, "100ps");
    }

    #[test]
    fn parse_scale_unit_empty_falls_back() {
        assert_eq!(parse_scale_unit("").1, "1ns");
    }

    #[test]
    fn parse_scale_unit_unknown_unit_falls_back() {
        assert_eq!(parse_scale_unit("1xx").1, "1ns");
    }

    #[test]
    fn parse_scale_unit_us_micro() {
        assert!((parse_scale_unit("1us").0 - 1e-6).abs() < 1e-20);
    }

    #[test]
    fn parse_scale_unit_handles_uppercase() {
        assert!((parse_scale_unit("10NS").0 - 10e-9).abs() < 1e-20);
    }

    #[test]
    fn split_scope_leaf_dot() {
        assert_eq!(split_scope_leaf("a.b.c"), ("a.b", "c"));
    }

    #[test]
    fn split_scope_leaf_slash() {
        assert_eq!(split_scope_leaf("a/b/c"), ("a/b", "c"));
    }

    #[test]
    fn split_scope_leaf_mixed_prefers_rightmost() {
        assert_eq!(split_scope_leaf("top/sub.leaf"), ("top/sub", "leaf"));
        assert_eq!(split_scope_leaf("top.sub/leaf"), ("top.sub", "leaf"));
    }

    #[test]
    fn split_scope_leaf_no_separator() {
        assert_eq!(split_scope_leaf("just_leaf"), ("", "just_leaf"));
    }
}
