// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Format support beyond the native core.
//!
//! rwave's [`crate::backend::wellen_backend`] handles VCD, FST, and GHW.
//! Everything else is bridged through a stable C ABI documented in
//! `docs/PLUGIN.md` and declared in `include/rwave_backend.h`. A backend's
//! vtable reaches rwave one of two ways:
//!
//! * **compiled in** — `wlf` and `fsdb` ship inside the rwave binary; see
//!   [`builtin`]. Their vendor libraries are still located at runtime via
//!   env var.
//! * **external** — any other extension `<ext>` is loaded from the cdylib
//!   named by `$RWAVE_PLUGIN_<EXT>`; see [`loader`]. An external plugin also
//!   *overrides* a built-in of the same extension (e.g. an external
//!   `.fsdb` backend takes precedence over the built-in NPI one).
//!
//! Layout:
//! * [`ffi`] — Rust mirror of the C ABI (`#[repr(C)]` structs). The single
//!   source of truth for the vtable layout; the built-in backends use these
//!   types directly.
//! * [`builtin`] — the compiled-in `wlf`/`fsdb` vtables (feature- and
//!   target-gated).
//! * [`loader`] — `$RWAVE_PLUGIN_<EXT>` discovery and the user-facing error
//!   strings.
//!
//! The adapter that drives a vtable (built-in or external) through rwave's
//! [`crate::backend::WaveformBackend`] trait lives in
//! [`crate::backend::plugin_backend`].

pub mod builtin;
pub mod ffi;
pub mod loader;

pub use loader::LoadError;
