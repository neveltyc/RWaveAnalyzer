// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! A read-only SQLite VFS that hides Questa's header.
//!
//! A `.dbg` is stock SQLite with its first 16 bytes overwritten: `Modelsim dbg
//! 1 \0` where SQLite looks for `SQLite format 3\0`. Reading the whole file into
//! memory to correct those bytes costs the size of the file — 1.34 GB on a real
//! design — before the first query can run.
//!
//! This does the correction one read at a time instead. The VFS is a shim: it
//! inherits every method of the platform's own VFS and overrides `xOpen` so that
//! each file it hands back has an `xRead` that overlays the right magic on any
//! read touching byte range [0, 16). SQLite then pages the file off disk the way
//! it pages any database, and resident memory is its page cache rather than the
//! whole file.
//!
//! The user's file is never written to. `xWrite` and `xTruncate` refuse on the
//! main database — belt as well as braces, since the connection is opened
//! read-only — while temporary files SQLite makes for itself pass through
//! untouched, so a query that needs to spill still works.
//!
//! Memory-mapped I/O is turned off for every file this VFS opens: `xFetch`
//! always declines. A mapped page 1 would carry Questa's magic, having never
//! gone through `xRead`, and SQLite would refuse the file it had just opened.

use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;

use rusqlite::ffi;

/// What the header should say.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// How far into the file the lie extends.
const HEADER: i64 = 16;

/// The name to hand `sqlite3_open_v2`.
pub const VFS_NAME: &CStr = c"rwave-questa-dbg";

/// The registered VFS, plus the one it delegates to.
///
/// `base` is a copy of the platform VFS, so every method rwave does not care
/// about — path resolution, randomness, the clock — is inherited verbatim,
/// including `pAppData`, which the unix VFS uses to find its own syscall table.
/// The delegate is kept after those fields instead, where `xOpen` can reach it
/// by casting.
#[repr(C)]
struct ShimVfs {
    base: ffi::sqlite3_vfs,
    root: *mut ffi::sqlite3_vfs,
}

// Registered once and never mutated afterwards; SQLite serialises its own
// access to the VFS list.
unsafe impl Sync for ShimVfs {}
unsafe impl Send for ShimVfs {}

/// One open file: the shim SQLite sees, and the real one behind it.
///
/// SQLite allocates `szOsFile` bytes and hands them over; this struct sits at
/// the front of that block and the platform VFS's own file structure directly
/// after it.
#[repr(C)]
struct ShimFile {
    base: ffi::sqlite3_file,
    real: *mut ffi::sqlite3_file,
    /// Whether this is the database whose header lies. Journals and temporary
    /// files opened through the same VFS are ordinary files.
    patch: c_int,
    _pad: c_int,
}

/// Register the VFS, once per process. Returns the name to open files with.
pub fn register() -> Result<&'static CStr, String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    let done = REGISTERED.get_or_init(|| unsafe {
        if ffi::sqlite3_initialize() != ffi::SQLITE_OK {
            return Err("SQLite would not initialise".to_string());
        }
        let root = ffi::sqlite3_vfs_find(std::ptr::null());
        if root.is_null() {
            return Err("SQLite has no default VFS to build on".to_string());
        }
        if (*root).xOpen.is_none() {
            return Err("the default VFS cannot open files".to_string());
        }

        let mut base = *root;
        base.pNext = std::ptr::null_mut();
        base.zName = VFS_NAME.as_ptr();
        base.szOsFile = size_of::<ShimFile>() as c_int + (*root).szOsFile;
        base.xOpen = Some(x_open);

        // Leaked deliberately: SQLite keeps the pointer for the life of the
        // process, and there is nothing to reclaim at exit.
        let shim: &'static mut ShimVfs = Box::leak(Box::new(ShimVfs { base, root }));
        let rc = ffi::sqlite3_vfs_register(&raw mut shim.base, 0);
        if rc != ffi::SQLITE_OK {
            return Err(format!("SQLite refused the reader VFS (code {rc})"));
        }
        Ok(())
    });
    match done {
        Ok(()) => Ok(VFS_NAME),
        Err(e) => Err(e.clone()),
    }
}

/// The real file sitting behind a shim.
#[inline]
unsafe fn real(p: *mut ffi::sqlite3_file) -> *mut ffi::sqlite3_file {
    unsafe { (*p.cast::<ShimFile>()).real }
}

/// The real file's method table.
#[inline]
unsafe fn methods(p: *mut ffi::sqlite3_file) -> *const ffi::sqlite3_io_methods {
    unsafe { (*real(p)).pMethods }
}

unsafe extern "C" fn x_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    unsafe {
        let root = (*vfs.cast::<ShimVfs>()).root;
        let p = file.cast::<ShimFile>();
        // Left null until the open succeeds: SQLite reads it to decide whether
        // the file is worth closing.
        (*p).base.pMethods = std::ptr::null();
        (*p).patch = c_int::from(flags & ffi::SQLITE_OPEN_MAIN_DB != 0);
        (*p)._pad = 0;
        let r = file.cast::<u8>().add(size_of::<ShimFile>()).cast::<ffi::sqlite3_file>();
        (*r).pMethods = std::ptr::null();
        (*p).real = r;

        let rc = ((*root).xOpen.unwrap())(root, name, r, flags, out_flags);
        if rc == ffi::SQLITE_OK {
            (*p).base.pMethods = &raw const METHODS;
        } else if !(*r).pMethods.is_null() {
            // A partial open still holds a descriptor.
            if let Some(close) = (*(*r).pMethods).xClose {
                close(r);
            }
            (*r).pMethods = std::ptr::null();
        }
        rc
    }
}

unsafe extern "C" fn x_close(p: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let r = real(p);
        match (*methods(p)).xClose {
            Some(f) => {
                let rc = f(r);
                (*r).pMethods = std::ptr::null();
                rc
            }
            None => ffi::SQLITE_OK,
        }
    }
}

/// The one method that exists for this VFS to exist.
unsafe extern "C" fn x_read(
    p: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    amt: c_int,
    ofst: ffi::sqlite3_int64,
) -> c_int {
    unsafe {
        let r = real(p);
        let rc = ((*methods(p)).xRead.unwrap())(r, buf, amt, ofst);
        // Only a complete read is corrected. A short one means the file ends
        // inside the range asked for, and the bytes to overlay may be bytes the
        // file does not have.
        if rc == ffi::SQLITE_OK && (*p.cast::<ShimFile>()).patch != 0 && ofst < HEADER {
            let from = ofst as usize;
            let n = ((HEADER - ofst) as usize).min(amt.max(0) as usize);
            let fix = SQLITE_MAGIC[from..from + n].as_ptr();
            std::ptr::copy_nonoverlapping(fix, buf.cast::<u8>(), n);
        }
        rc
    }
}

unsafe extern "C" fn x_write(
    p: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amt: c_int,
    ofst: ffi::sqlite3_int64,
) -> c_int {
    unsafe {
        if (*p.cast::<ShimFile>()).patch != 0 {
            // A simulation output is not rwave's to change.
            return ffi::SQLITE_READONLY;
        }
        let r = real(p);
        ((*methods(p)).xWrite.unwrap())(r, buf, amt, ofst)
    }
}

unsafe extern "C" fn x_truncate(p: *mut ffi::sqlite3_file, size: ffi::sqlite3_int64) -> c_int {
    unsafe {
        if (*p.cast::<ShimFile>()).patch != 0 {
            return ffi::SQLITE_READONLY;
        }
        let r = real(p);
        ((*methods(p)).xTruncate.unwrap())(r, size)
    }
}

unsafe extern "C" fn x_sync(p: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    unsafe {
        if (*p.cast::<ShimFile>()).patch != 0 {
            // Nothing was written, so there is nothing to flush.
            return ffi::SQLITE_OK;
        }
        let r = real(p);
        ((*methods(p)).xSync.unwrap())(r, flags)
    }
}

unsafe extern "C" fn x_file_size(
    p: *mut ffi::sqlite3_file,
    size: *mut ffi::sqlite3_int64,
) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xFileSize.unwrap())(r, size)
    }
}

unsafe extern "C" fn x_lock(p: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xLock.unwrap())(r, level)
    }
}

unsafe extern "C" fn x_unlock(p: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xUnlock.unwrap())(r, level)
    }
}

unsafe extern "C" fn x_check_reserved_lock(p: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xCheckReservedLock.unwrap())(r, out)
    }
}

unsafe extern "C" fn x_file_control(
    p: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xFileControl.unwrap())(r, op, arg)
    }
}

unsafe extern "C" fn x_sector_size(p: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xSectorSize.unwrap())(r)
    }
}

unsafe extern "C" fn x_device_characteristics(p: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let r = real(p);
        ((*methods(p)).xDeviceCharacteristics.unwrap())(r)
    }
}

/// Whether the real file's methods reach as far as the shared-memory calls.
#[inline]
unsafe fn shm_capable(p: *mut ffi::sqlite3_file) -> bool {
    unsafe { (*methods(p)).iVersion >= 2 }
}

unsafe extern "C" fn x_shm_map(
    p: *mut ffi::sqlite3_file,
    pg: c_int,
    pgsz: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    unsafe {
        if !shm_capable(p) {
            return ffi::SQLITE_IOERR;
        }
        let r = real(p);
        match (*methods(p)).xShmMap {
            Some(f) => f(r, pg, pgsz, extend, out),
            None => ffi::SQLITE_IOERR,
        }
    }
}

unsafe extern "C" fn x_shm_lock(
    p: *mut ffi::sqlite3_file,
    offset: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    unsafe {
        if !shm_capable(p) {
            return ffi::SQLITE_IOERR;
        }
        let r = real(p);
        match (*methods(p)).xShmLock {
            Some(f) => f(r, offset, n, flags),
            None => ffi::SQLITE_IOERR,
        }
    }
}

unsafe extern "C" fn x_shm_barrier(p: *mut ffi::sqlite3_file) {
    unsafe {
        if !shm_capable(p) {
            return;
        }
        let r = real(p);
        if let Some(f) = (*methods(p)).xShmBarrier {
            f(r);
        }
    }
}

unsafe extern "C" fn x_shm_unmap(p: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    unsafe {
        if !shm_capable(p) {
            return ffi::SQLITE_IOERR;
        }
        let r = real(p);
        match (*methods(p)).xShmUnmap {
            Some(f) => f(r, delete),
            None => ffi::SQLITE_IOERR,
        }
    }
}

/// Decline every mapping request, which is how a VFS says "read it instead".
///
/// This is what keeps the correction honest: a mapped page would come straight
/// from the file, Questa's magic and all.
unsafe extern "C" fn x_fetch(
    _p: *mut ffi::sqlite3_file,
    _ofst: ffi::sqlite3_int64,
    _amt: c_int,
    out: *mut *mut c_void,
) -> c_int {
    unsafe {
        *out = std::ptr::null_mut();
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_unfetch(
    _p: *mut ffi::sqlite3_file,
    _ofst: ffi::sqlite3_int64,
    _page: *mut c_void,
) -> c_int {
    ffi::SQLITE_OK
}

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 3,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: Some(x_shm_map),
    xShmLock: Some(x_shm_lock),
    xShmBarrier: Some(x_shm_barrier),
    xShmUnmap: Some(x_shm_unmap),
    xFetch: Some(x_fetch),
    xUnfetch: Some(x_unfetch),
};
