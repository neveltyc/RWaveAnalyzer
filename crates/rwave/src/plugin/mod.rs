// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! External format support via dynamically loaded plugins.
//!
//! rwave's built-in [`crate::backend::wellen_backend`] handles VCD, FST,
//! and GHW. Other formats are loaded at runtime from plugin shared
//! libraries that conform to the stable C ABI documented in
//! `docs/PLUGIN.md` and declared in `include/rwave_backend.h`.
//!
//! Layout:
//! * [`ffi`] — Rust mirror of the C ABI (`#[repr(C)]` structs).
//! * [`platform`] — compile-time platform-support flag and wheel
//!   platform tag. On unsupported targets, [`platform::SUPPORTED`] is
//!   `false` and the loader short-circuits to a clean error.
//! * [`loader`] — env-var / site-packages discovery and the user-facing
//!   error messages. Discovery is keyed on the format token (the file
//!   extension); rwave keeps no registry of which plugins exist.
//!
//! The actual backend implementation that bridges a loaded plugin into
//! rwave's [`crate::backend::WaveformBackend`] trait lives in
//! [`crate::backend::plugin_backend`], not here — this module stays the
//! "plumbing", that one is the trait adapter.

pub mod ffi;
pub mod loader;
pub mod platform;

pub use loader::LoadError;
pub use platform::{PLATFORM_TAG, SUPPORTED};
