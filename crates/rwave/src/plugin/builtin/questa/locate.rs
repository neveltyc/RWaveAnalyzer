// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Finding `vsim`. Two places, then an error.
//!
//! Beside `$RWAVE_WLF_LIB` first: reading a `.wlf` at all requires pointing that
//! at the vendor library, and in a Questa install `libwlf.so` and `vsim` are
//! siblings, so the answer is usually already configured. Then `PATH`. A user
//! who pointed the variable at a library copied out of its install still gets
//! the `PATH` hit, so a miss beside the library falls through rather than
//! failing.
//!
//! No other probing. Guessing `../bin` or a `$QUESTA_HOME` would risk answering
//! from a different Questa than the one that wrote the waveform.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::err;

/// Executable name on this platform.
pub fn vsim_name() -> &'static str {
    if cfg!(windows) { "vsim.exe" } else { "vsim" }
}

/// Locate `vsim` using the real environment.
pub fn locate_vsim() -> Result<PathBuf, String> {
    let lib = std::env::var_os("RWAVE_WLF_LIB").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    locate_vsim_with(lib.as_deref(), path.as_deref(), &|p| is_executable(p))
}

/// The probe itself, with the filesystem injected so it can be tested without
/// touching the environment.
pub fn locate_vsim_with(
    wlf_lib: Option<&Path>,
    path_var: Option<&OsStr>,
    is_exec: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    let mut lib_dir = None;
    if let Some(dir) = wlf_lib.and_then(Path::parent).filter(|d| !d.as_os_str().is_empty()) {
        let cand = dir.join(vsim_name());
        if is_exec(&cand) {
            return Ok(cand);
        }
        lib_dir = Some(dir.to_path_buf());
    }
    if let Some(var) = path_var {
        for dir in std::env::split_paths(var) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let cand = dir.join(vsim_name());
            if is_exec(&cand) {
                return Ok(cand);
            }
        }
    }
    let looked = match &lib_dir {
        Some(d) => format!("beside RWAVE_WLF_LIB ({}) and on PATH", d.display()),
        None => "on PATH (RWAVE_WLF_LIB is not set)".to_string(),
    };
    Err(err(format!(
        "cannot find {} — looked {looked}. trace on a WLF answers from Questa's debug \
         database by running `vsim -c -view`, so set RWAVE_WLF_LIB to the libwlf in your \
         Questa installation, or put vsim on PATH.",
        vsim_name()
    )))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn only(present: &'static str) -> impl Fn(&Path) -> bool {
        move |p: &Path| p == Path::new(present)
    }

    fn joined(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs.iter().map(Path::new)).unwrap()
    }

    #[test]
    fn takes_the_sibling_of_the_configured_vendor_library() {
        let lib = PathBuf::from("/q/10.7c/linux_x86_64/libwlf.so");
        let want = format!("/q/10.7c/linux_x86_64/{}", vsim_name());
        let got = locate_vsim_with(Some(&lib), None, &|p| p == Path::new(&want)).unwrap();
        assert_eq!(got, PathBuf::from(&want));
    }

    #[test]
    fn a_relocated_library_still_gets_the_path_hit() {
        // RWAVE_WLF_LIB may point at a library copied out of its install, which
        // is why a miss beside it falls through instead of failing.
        let lib = PathBuf::from("/opt/copied/libwlf.so");
        let want = format!("/usr/local/questa/{}", vsim_name());
        let got = locate_vsim_with(
            Some(&lib),
            Some(&joined(&["/bin", "/usr/local/questa"])),
            &only(Box::leak(want.clone().into_boxed_str())),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from(&want));
    }

    #[test]
    fn the_error_names_both_places_it_looked() {
        let lib = PathBuf::from("/opt/copied/libwlf.so");
        let e = locate_vsim_with(Some(&lib), Some(&joined(&["/bin"])), &|_| false).unwrap_err();
        assert!(e.contains("/opt/copied"), "{e}");
        assert!(e.contains("PATH"), "{e}");
        assert!(e.contains("RWAVE_WLF_LIB"), "{e}");
    }

    #[test]
    fn without_the_variable_the_error_says_so() {
        let e = locate_vsim_with(None, Some(&joined(&["/bin"])), &|_| false).unwrap_err();
        assert!(e.contains("RWAVE_WLF_LIB is not set"), "{e}");
    }
}
