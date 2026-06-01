// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Compile-time platform support for plugin loading.
//!
//! rwave attempts plugin loading only on amd64 Linux and Windows by
//! current convention. On other targets the entire loader code path
//! short-circuits to a clean "not supported on this platform" error
//! without attempting any filesystem or process work — see
//! [`super::loader`].
//!
//! The cfg gates here are mirrored exactly in [`super::loader`] so the two
//! files compile to consistent stubs on every target.

/// True iff this rwave build attempts plugin loading at all.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
))]
pub const SUPPORTED: bool = true;

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
)))]
pub const SUPPORTED: bool = false;

/// PEP 425 platform tag used in error messages and the wheel filename
/// rwave tells users to install. Empty on unsupported platforms (where
/// it is never read — the unsupported-platform branch fires first).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const PLATFORM_TAG: &str = "linux_x86_64";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const PLATFORM_TAG: &str = "win_amd64";

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
)))]
pub const PLATFORM_TAG: &str = "";
