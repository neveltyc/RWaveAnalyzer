use std::ffi::CString;

pub fn bridge_err(msg: impl AsRef<str>) -> String {
    format!("rwave-fsdb: {}", msg.as_ref())
}

/// Replace interior NULs with '?' so the conversion never fails.
pub fn to_cstring(s: impl AsRef<str>) -> CString {
    let bytes: Vec<u8> = s
        .as_ref()
        .bytes()
        .map(|b| if b == 0 { b'?' } else { b })
        .collect();
    unsafe { CString::from_vec_unchecked(bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_err_adds_prefix() {
        assert_eq!(bridge_err("oops"), "rwave-fsdb: oops");
    }

    #[test]
    fn to_cstring_replaces_interior_nul() {
        assert_eq!(to_cstring("foo\0bar").to_bytes(), b"foo?bar");
    }
}
