// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! External-plugin discovery and the user-facing plugin error strings.
//!
//! Built-in formats need no discovery: `vcd`/`fst`/`ghw` are handled
//! natively by [`crate::backend::wellen_backend`], and `wlf`/`fsdb` by the
//! compiled-in backends in [`crate::plugin::builtin`]. Any *other* extension
//! `<ext>` is served by an external backend cdylib whose absolute path the
//! user gives in `$RWAVE_PLUGIN_<EXT>`. There is no search path and no
//! registry — one env var, one `.so`.

use std::path::PathBuf;

/// Failure modes surfaced when resolving a backend for an extension. The
/// [`Display`] impl is the authoritative source of the user-facing strings.
#[derive(Debug)]
pub enum LoadError {
    /// No built-in backend for this extension and `$RWAVE_PLUGIN_<EXT>` is
    /// unset (or names a file that does not exist).
    NoBackend { format: String },
    /// A built-in format (`wlf`/`fsdb`) was requested, but this build does
    /// not include it — the target isn't one the vendor library exists for,
    /// or the per-format feature was disabled at build time. `platforms`
    /// names where it *is* available.
    BuiltinUnavailable {
        format: String,
        platforms: &'static str,
    },
    /// An external plugin loaded but its `abi_version` differs from rwave's.
    /// Separate variant so the message can name the specific remediation.
    AbiMismatch {
        format: String,
        plugin_abi: u32,
        rwave_abi: u32,
    },
    /// A candidate was found (an external `.so`, or a built-in's vendor
    /// library) but loading / initialisation failed. The string is the
    /// diagnostic — `dlerror`, the plugin's `err_out`, or the built-in's
    /// init error — passed through verbatim.
    LoadFailed { msg: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NoBackend { format } => write!(
                f,
                "no backend for .{format} files. Set RWAVE_PLUGIN_{upper} to a \
                 backend library path to handle this format (see docs/PLUGIN.md).",
                upper = format.to_ascii_uppercase(),
            ),
            LoadError::BuiltinUnavailable { format, platforms } => write!(
                f,
                "{format} support is only available in the {platforms} build."
            ),
            LoadError::AbiMismatch {
                format,
                plugin_abi,
                rwave_abi,
            } => write!(
                f,
                "{format} backend ABI mismatch (plugin v{plugin_abi}, rwave \
                 expects v{rwave_abi}). Rebuild the backend against rwave's \
                 current ABI."
            ),
            LoadError::LoadFailed { msg } => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Absolute path to the external backend for `format`, taken from
/// `$RWAVE_PLUGIN_<EXT>` (the extension uppercased). Returns `None` if the
/// var is unset or names a path that is not an existing file — the caller
/// then falls back to the built-in resolver.
pub fn external_plugin_path(format: &str) -> Option<PathBuf> {
    let var = format!("RWAVE_PLUGIN_{}", format.to_ascii_uppercase());
    let path = PathBuf::from(std::env::var_os(var)?);
    path.is_file().then_some(path)
}

// ---------------------------------------------------------------------------
// Tests (error formatting)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_backend_message_names_the_env_var_and_omits_version() {
        let e = LoadError::NoBackend {
            format: "foo".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains("no backend for .foo"));
        assert!(s.contains("RWAVE_PLUGIN_FOO"));
        // The rwave version must never leak into a backend-discovery hint:
        // backend availability is independent of the analyzer's version.
        assert!(!s.contains(crate::VERSION));
    }

    #[test]
    fn builtin_unavailable_message() {
        let e = LoadError::BuiltinUnavailable {
            format: "fsdb".to_string(),
            platforms: "linux-x86_64",
        };
        assert_eq!(
            e.to_string(),
            "fsdb support is only available in the linux-x86_64 build."
        );
    }

    #[test]
    fn abi_mismatch_mentions_both_versions() {
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
    fn load_failed_passthrough() {
        let e = LoadError::LoadFailed {
            msg: "libfoo.so: undefined symbol bar".to_string(),
        };
        assert_eq!(e.to_string(), "libfoo.so: undefined symbol bar");
    }
}
