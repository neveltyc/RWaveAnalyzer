// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Plugin discovery and user-facing error messages.
//!
//! rwave does not maintain a registry of which extensions go to which
//! plugin. The convention is: file extension `<ext>` routes to the
//! plugin whose package directory is `rwave_<ext>/` and whose shared
//! library is `librwave_<ext>_backend.{so,dll}`. Adding a new format is
//! a plugin-side concern — the plugin author picks the package name; no
//! rwave code change is required.
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
/// 2. `$VIRTUAL_ENV/{lib,Lib}/python3.*/site-packages/rwave_<format>/...`.
/// 3. `~/.local/lib/python3.*/site-packages/rwave_<format>/...`.
///
/// System-wide installs (e.g. system Python's site-packages) are left
/// to the env-var escape hatch — querying them would require spawning
/// `python3 -c "import sysconfig; ..."`, which we want to keep off the
/// open path until there is real demand.
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

    // (root, libdir-name)
    let roots: Vec<(PathBuf, &'static str)> = collect_roots();

    for (root, libdir) in roots {
        let dir = root.join(libdir);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Match python3, python3.9, python3.10, ... Skip python2 etc.
            if name_str == "python3" || name_str.starts_with("python3.") {
                let cand = entry.path().join("site-packages").join(&pkg).join(&libname);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
))]
fn collect_roots() -> Vec<(PathBuf, &'static str)> {
    let mut roots: Vec<(PathBuf, &'static str)> = Vec::new();

    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        // Unix venvs use lib/, Windows venvs use Lib/. Try both — empty
        // ones get skipped by the read_dir.
        roots.push((PathBuf::from(&venv), "lib"));
        roots.push((PathBuf::from(&venv), "Lib"));
    }

    if let Some(home) = std::env::var_os("HOME") {
        roots.push((PathBuf::from(home).join(".local"), "lib"));
    }
    // Windows user-site: %APPDATA%\Python\Python3X\site-packages\... — the
    // structure differs enough that we leave it to the env var override
    // for now rather than enumerate Python versions blindly.

    roots
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn libname_for(format: &str) -> String {
    format!("librwave_{format}_backend.so")
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn libname_for(format: &str) -> String {
    format!("librwave_{format}_backend.dll")
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
