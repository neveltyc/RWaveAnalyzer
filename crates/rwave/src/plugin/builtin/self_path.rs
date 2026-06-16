// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Locate this binary/cdylib's own on-disk directory, so a built-in backend can
//! find its vendor library (`libwlf` / `libNPI`) as a sibling of the rwave
//! executable. Shared by the WLF and FSDB backends.
//!
//! The marker function resolves — via `dladdr` (unix) / `GetModuleHandleEx`
//! (windows) — back to the module that contains it. Because the built-in
//! backends are compiled into the rwave binary, that resolves to the rwave
//! executable's own path, exactly as each backend's private copy did before.

use std::path::PathBuf;

/// Directory containing this module's binary/cdylib, if resolvable.
pub fn self_dir() -> Option<PathBuf> {
    self_path()?.parent().map(|d| d.to_path_buf())
}

/// Marker function whose address `dladdr` / `GetModuleHandleEx` resolves back to
/// this module's file path. Has to be defined in-crate; the body is trivial.
fn self_marker() {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn self_path() -> Option<PathBuf> {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }
    unsafe extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    }

    let marker: *const c_void = self_marker as *const () as *const c_void;
    let mut info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    // SAFETY: dladdr is a standard libdl entry point; marker is a valid
    // function pointer; info is a valid writable struct.
    let rc = unsafe { dladdr(marker, &mut info) };
    if rc == 0 || info.dli_fname.is_null() {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }
        .to_str()
        .ok()?;
    Some(PathBuf::from(s))
}

#[cfg(target_os = "windows")]
fn self_path() -> Option<PathBuf> {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;

    type HModule = *mut c_void;
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, addr: *const u16, h_out: *mut HModule) -> i32;
        fn GetModuleFileNameW(h: HModule, buf: *mut u16, size: u32) -> u32;
    }
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;

    let marker: *const c_void = self_marker as *const () as *const c_void;
    let mut h: HModule = std::ptr::null_mut();
    // SAFETY: addr is a valid function pointer; h is a writable handle slot.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            marker as *const u16,
            &mut h,
        )
    };
    if ok == 0 || h.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 32768];
    // SAFETY: h obtained above; buf is a writable WCHAR array.
    let n = unsafe { GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let os_str = OsString::from_wide(&buf[..n as usize]);
    Some(PathBuf::from(os_str))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn self_path() -> Option<PathBuf> {
    None
}
