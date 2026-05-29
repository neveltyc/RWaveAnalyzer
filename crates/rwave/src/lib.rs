// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `rwave` — an AI-agent-friendly waveform analyzer for RTL debug.
//!
//! ## Architecture (layers, top to bottom)
//!
//! * [`cli`] — argument parsing only; no I/O, no domain logic.
//! * [`commands`] — presentation and per-command logic. Talks to the domain
//!   model and the leaf utilities; never touches a parser backend directly.
//! * [`model`] — the format-neutral domain: a signal table, value-change
//!   replay, and snapshots. Owns a backend behind a trait object.
//! * [`backend`] — the parser abstraction. [`backend::WaveformBackend`] is the
//!   contract; [`backend::wellen_backend`] is the default implementation (VCD /
//!   FST / GHW via the `wellen` crate). Adding a format = adding a backend.
//!
//! Leaf utilities, depended on by `commands` but not by each other or by any
//! backend: [`format`] (value/time formatting and parsing), [`filter`] (signal
//! pattern matching), [`condition`] (search predicates), [`json`] (compact
//! serializer). Keeping these backend-agnostic is what lets the analyzer grow
//! to new waveform formats without rippling changes through the command set.
//!
//! The CLI surface and output shapes are drop-in compatible with the reference
//! `vcd_analyzer.py`, generalized so that VCD-specific wording becomes
//! format-neutral where a feature applies to all backends.

pub mod backend;
pub mod cli;
pub mod commands;
pub mod condition;
pub mod filter;
pub mod format;
pub mod json;
pub mod model;

/// Version reported by `rwave --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
