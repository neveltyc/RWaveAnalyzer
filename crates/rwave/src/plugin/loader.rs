// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Plugin discovery and user-facing error messages.
//!
//! rwave does not maintain a registry of which extensions go to which
//! plugin. The convention is: file extension `<ext>` routes to the
//! plugin whose package directory is `rwave_<ext>/` and whose shared
//! library is `librwave_<ext>_backend.so` on Linux / `rwave_<ext>_backend.dll`
//! on Windows (Windows cdylibs carry no `lib` prefix — that's a Unix
//! convention). Adding a new format is a plugin-side concern — the plugin
//! author picks the package name; no rwave code change is required.
//!
//! ## What lives here
//!
//! * [`locate_plugin`] — the discovery walk (env var, then site-packages
//!   scan), keyed on the format token (the lowercase extension).
//! * [`LoadError`] — the failure shapes. Its [`Display`] impl produces
//!   the exact strings rwave's CLI prints. The platform tag is the only
//!   build-time token interpolated into the "not installed" hint; the
//!   rwave version is deliberately absent.

use std::path::PathBuf;

use crate::plugin::platform::{PLATFORM_TAG, SUPPORTED};

/// Failure modes of the plugin pipeline. The [`Display`] impl is the
/// authoritative source of the user-facing error strings.
///
/// The wheel filename rwave hints at is intentionally version-agnostic:
/// the rwave binary, the plugin's own semver, and the binary ABI
/// version are three independent things. Coupling rwave's version into
/// the install hint would falsely imply that bumping rwave forces a
/// wheel rebuild, which it does not unless the ABI itself bumps.
#[derive(Debug)]
pub enum LoadError {
    /// Platform is not in the plugin-supported set.
    PlatformUnsupported { format: String },
    /// No plugin shared library found at any of the discovery paths.
    NotInstalled { format: String },
    /// Plugin loaded but its `abi_version` does not match what rwave
    /// expects. Separate variant (not `LoadFailed`) so the message can
    /// be specific — this is a different remediation than "I can't
    /// open your .so".
    AbiMismatch {
        format: String,
        plugin_abi: u32,
        rwave_abi: u32,
    },
    /// Found a candidate but loading / initialisation failed. The string
    /// is the diagnostic — either `dlerror` output or the plugin's own
    /// `err_out` payload, passed through verbatim.
    LoadFailed { msg: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::PlatformUnsupported { format } => {
                write!(
                    f,
                    "{format} extension is not supported on this platform."
                )
            }
            LoadError::NotInstalled { format } => {
                write!(
                    f,
                    "{format} support not installed. \
                     Install a rwave_{format} wheel for {plat}.",
                    plat = PLATFORM_TAG,
                )
            }
            LoadError::AbiMismatch {
                format,
                plugin_abi,
                rwave_abi,
            } => {
                write!(
                    f,
                    "{format} backend ABI mismatch \
                     (plugin v{plugin_abi}, rwave expects v{rwave_abi}). \
                     Reinstall a rwave_{format} wheel matching rwave's ABI version."
                )
            }
            LoadError::LoadFailed { msg } => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Locate the plugin shared library for the given format.
///
/// Order:
/// 1. `$RWAVE_PLUGIN_<FORMAT>` (uppercase), if set and the file exists.
/// 2. The active virtualenv's site-packages, if `$VIRTUAL_ENV` is set.
/// 3. The per-user site-packages:
///    * Linux:   `~/.local/lib/python3.*/site-packages/`
///    * Windows: `%APPDATA%\Python\Python3XX\site-packages/` (where
///      `pip install --user` lands).
///
/// The probed path is `<site-packages>/rwave_<format>/<libname>`, where
/// `<libname>` is `librwave_<format>_backend.so` on Linux and
/// `rwave_<format>_backend.dll` on Windows. System-wide installs are
/// left to the env-var escape hatch.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
))]
pub fn locate_plugin(format: &str) -> Result<PathBuf, LoadError> {
    let _ = SUPPORTED; // compile-time assert via const eval

    let env_var = format!("RWAVE_PLUGIN_{}", format.to_ascii_uppercase());
    if let Some(p) = std::env::var_os(&env_var) {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        // Env var set but file missing — fall through. The env var
        // points at *this* plugin, so if it's wrong we still report
        // NotInstalled rather than pretend the user didn't try.
    }

    if let Some(p) = scan_site_packages(format) {
        return Ok(p);
    }

    Err(LoadError::NotInstalled {
        format: format.to_string(),
    })
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
)))]
pub fn locate_plugin(format: &str) -> Result<PathBuf, LoadError> {
    let _ = SUPPORTED; // touches the symbol so an unused-import warning never fires
    Err(LoadError::PlatformUnsupported {
        format: format.to_string(),
    })
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
))]
fn scan_site_packages(format: &str) -> Option<PathBuf> {
    let pkg = format!("rwave_{format}");
    let libname = libname_for(format);

    for site_packages in candidate_site_packages() {
        let cand = site_packages.join(&pkg).join(&libname);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Resolved `site-packages` directories to probe, most-specific first.
/// Platform-specific: a Python install's on-disk layout differs between
/// Unix and Windows (directory names, whether there is a `pythonX.Y`
/// level, and where `pip install --user` lands).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn candidate_site_packages() -> Vec<PathBuf> {
    // Unix layout: <lib-root>/python3.*/site-packages.
    let mut lib_roots: Vec<PathBuf> = Vec::new();
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        lib_roots.push(PathBuf::from(venv).join("lib"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        lib_roots.push(PathBuf::from(home).join(".local").join("lib"));
    }

    let mut out = Vec::new();
    for root in lib_roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            // python3, python3.9, python3.10, ...  (skip python2.*)
            if s == "python3" || s.starts_with("python3.") {
                out.push(entry.path().join("site-packages"));
            }
        }
    }
    out
}

/// See the Linux variant. On Windows the layout is flatter: a venv keeps
/// packages in `<venv>\Lib\site-packages` (no `pythonX.Y` level), and
/// `pip install --user` lands in `%APPDATA%\Python\Python3XX\site-packages`
/// (`%APPDATA%` already resolves to the Roaming profile).
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn candidate_site_packages() -> Vec<PathBuf> {
    let mut out = Vec::new();

    // Active virtualenv: <venv>\Lib\site-packages.
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        out.push(PathBuf::from(venv).join("Lib").join("site-packages"));
    }

    // pip --user: %APPDATA%\Python\Python3XX\site-packages. Enumerate the
    // Python3XX dirs rather than guess the interpreter version.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let pyroot = PathBuf::from(appdata).join("Python");
        if let Ok(entries) = std::fs::read_dir(&pyroot) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("Python3") {
                    out.push(entry.path().join("site-packages"));
                }
            }
        }
    }

    out
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn libname_for(format: &str) -> String {
    format!("librwave_{format}_backend.so")
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn libname_for(format: &str) -> String {
    // Windows cdylibs carry no `lib` prefix (that's a Unix convention),
    // so the wheel ships `rwave_<fmt>_backend.dll`, not `librwave_...`.
    format!("rwave_{format}_backend.dll")
}

// ---------------------------------------------------------------------------
// Tests (cfg-independent: error formatting)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_platform_unsupported_message() {
        let e = LoadError::PlatformUnsupported {
            format: "foo".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "foo extension is not supported on this platform."
        );
    }

    #[test]
    fn error_not_installed_message_is_version_agnostic() {
        // Critical invariant: rwave's own version must NOT appear in the
        // install hint. Wheel version and rwave version are independent
        // semantics (see LoadError doc comment).
        let e = LoadError::NotInstalled {
            format: "foo".to_string(),
        };
        let s = e.to_string();
        assert!(s.starts_with("foo support not installed."));
        assert!(s.contains("rwave_foo"));
        assert!(!s.contains(crate::VERSION));
    }

    #[test]
    fn error_abi_mismatch_message_mentions_both_versions() {
        let e = LoadError::AbiMismatch {
            format: "foo".to_string(),
            plugin_abi: 1,
            rwave_abi: 2,
        };
        let s = e.to_string();
        assert!(s.contains("ABI mismatch"));
        assert!(s.contains("plugin v1"));
        assert!(s.contains("rwave expects v2"));
    }

    #[test]
    fn error_load_failed_passthrough() {
        let e = LoadError::LoadFailed {
            msg: "libfoo.so: undefined symbol bar".to_string(),
        };
        assert_eq!(e.to_string(), "libfoo.so: undefined symbol bar");
    }
}
