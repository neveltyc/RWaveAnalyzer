// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Error-string helpers shared by the built-in WLF and FSDB backends.
//!
//! Every error path eventually surfaces a `String` that the ABI layer converts
//! to a heap `CString` and hands to rwave via the vtable's `err_out` mechanism.
//! We keep the strings short and prefixed (`rwave-wlf:` / `rwave-fsdb:`, each
//! backend supplies its own prefix via `ERR_PREFIX`) so they're identifiable in
//! mixed logs, and reach into the vendor library's own diagnostic when relevant.

use std::ffi::{c_char, CStr, CString};

/// Wrap a fallback message with a backend-specific `prefix` (e.g. `rwave-wlf`).
pub fn bridge_err(prefix: &str, msg: impl AsRef<str>) -> String {
    format!("{}: {}", prefix, msg.as_ref())
}

/// Read a vendor diagnostic pointer into a String, or fall back if NULL.
pub fn mentor_diag(p: *const c_char, fallback: &str) -> String {
    if p.is_null() {
        return fallback.to_string();
    }
    // SAFETY: caller asserts pointer is a NUL-terminated string owned by the
    // vendor library.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s.into_owned()
    }
}

/// Convert a Rust string to an owned `CString`, replacing interior NULs with
/// `?` so the conversion never fails.
pub fn to_cstring(s: impl AsRef<str>) -> CString {
    let bytes: Vec<u8> = s
        .as_ref()
        .bytes()
        .map(|b| if b == 0 { b'?' } else { b })
        .collect();
    // SAFETY: we just removed every interior NUL.
    unsafe { CString::from_vec_unchecked(bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_err_adds_prefix() {
        assert_eq!(bridge_err("rwave-wlf", "oops"), "rwave-wlf: oops");
        assert_eq!(
            bridge_err("rwave-fsdb", format!("rc={}", 7)),
            "rwave-fsdb: rc=7"
        );
    }

    #[test]
    fn to_cstring_passes_through_clean_input() {
        assert_eq!(to_cstring("hello").to_bytes(), b"hello");
        assert_eq!(to_cstring("").to_bytes(), b"");
    }

    #[test]
    fn to_cstring_replaces_interior_nul_with_question_mark() {
        assert_eq!(to_cstring("foo\0bar").to_bytes(), b"foo?bar");
        assert_eq!(to_cstring("\0\0\0").to_bytes(), b"???");
    }

    #[test]
    fn mentor_diag_handles_null_with_fallback() {
        assert_eq!(mentor_diag(std::ptr::null(), "fallback"), "fallback");
    }

    #[test]
    fn mentor_diag_reads_valid_cstr() {
        let c = CString::new("a real diag").unwrap();
        let s = mentor_diag(c.as_ptr(), "fallback");
        assert_eq!(s, "a real diag");
    }
}
