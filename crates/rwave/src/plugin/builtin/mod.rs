// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Compiled-in ("built-in") backends.
//!
//! `wlf` and `fsdb` are bridged through the *same* C-ABI vtable +
//! [`crate::backend::plugin_backend::PluginBackend`] adapter as an external
//! plugin — the only difference is that their vtable is compiled into rwave
//! instead of `dlopen`ed. Each backend loads its vendor library
//! (`libwlf` / `libNPI`) at init via its own env var, exactly as the
//! standalone plugin did; the backend code is copied near-verbatim and only
//! its ABI boundary is rewired to rwave's in-crate [`crate::plugin::ffi`]
//! types.
//!
//! These are **experimental, amd64-only** features, gated on a per-format
//! Cargo feature (on by default) *and* on target. **FSDB** is on `x86_64`
//! linux. **WLF** compiles in on `x86_64` linux *and* windows, but only
//! linux is publicly advertised: on Windows, Mentor ships `libwlf` as a
//! static `libwlf.lib` (not a runtime-loadable DLL), so a loadable `libwlf`
//! must be produced out-of-band — the windows WLF backend is kept for that
//! internal use but is not surfaced in the platform-support error (see
//! [`supported_platforms`]). On any other target — or `--no-default-features`
//! — they compile out, leaving pure VCD/FST/GHW with no proprietary surface.

use crate::plugin::ffi::RwaveBackend;

#[cfg(all(feature = "wlf", target_arch = "x86_64", any(target_os = "linux", target_os = "windows")))]
pub mod wlf;
#[cfg(all(feature = "fsdb", target_os = "linux", target_arch = "x86_64"))]
pub mod fsdb;

/// Format tokens that *a* rwave build can serve from a compiled-in backend,
/// independent of whether *this* build actually includes them. Lets the
/// resolver tell "known format, just not in this build" apart from "unknown
/// format".
pub const BUILTIN_FORMATS: &[&str] = &["wlf", "fsdb"];

/// Platforms where a built-in format is *advertised*, for the "not in this
/// build" error. Both are `linux-x86_64`. (WLF is also compiled into the
/// windows-x86_64 binary for internal use, but Windows has no runtime-loadable
/// vendor `libwlf` — Mentor ships a static `.lib` — so it is not advertised.)
pub fn supported_platforms(_format: &str) -> &'static str {
    "linux-x86_64"
}

/// Why a built-in vtable could not be produced for an extension.
pub enum BuiltinError {
    /// `format` is not a built-in token at all — the caller should look
    /// elsewhere (external plugin) or report an unknown format.
    NotBuiltin,
    /// `format` is a built-in token, but not compiled into this build (the
    /// target isn't one the vendor library exists for, or the feature was
    /// disabled). See [`supported_platforms`].
    Unavailable,
    /// Compiled in, but the vendor library failed to load / initialise
    /// (e.g. its `RWAVE_*_LIB` env var is unset or names a missing file).
    InitFailed(String),
}

/// Resolve the compiled-in vtable for `format`, running the backend's
/// one-time vendor-library load. See [`BuiltinError`] for the failure cases.
pub fn vtable(format: &str) -> Result<&'static RwaveBackend, BuiltinError> {
    match format {
        #[cfg(all(feature = "wlf", target_arch = "x86_64", any(target_os = "linux", target_os = "windows")))]
        "wlf" => wlf::vtable().map_err(BuiltinError::InitFailed),
        #[cfg(all(feature = "fsdb", target_os = "linux", target_arch = "x86_64"))]
        "fsdb" => fsdb::vtable().map_err(BuiltinError::InitFailed),
        _ if BUILTIN_FORMATS.contains(&format) => Err(BuiltinError::Unavailable),
        _ => Err(BuiltinError::NotBuiltin),
    }
}
