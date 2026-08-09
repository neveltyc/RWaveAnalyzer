// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Driving QuestaSim's `vsim` as a subprocess, to answer connectivity questions
//! about a `.wlf`.
//!
//! A WLF records values over time and carries no connectivity at all. Questa
//! keeps that in a separate post-simulation debug database (`.dbg`, written by
//! `vopt -debugdb`), whose format is proprietary and has no callable API: its
//! only parser is statically linked inside the `vish` executable, which is a
//! non-PIE `ET_EXEC` and so cannot be loaded into another process, and the
//! `dbg_*` reader symbols appear in none of the shipped shared libraries. So
//! the only way in is to run `vsim -c -view` and drive its Tcl commands.
//!
//! Nothing here touches FFI or a vendor library — it is `std::process` and
//! string handling — so unlike the `wlf` module this carries no target gate and
//! its tests run on every platform. Only the [`DesignQuery`] implementation
//! that uses it is gated.
//!
//! [`DesignQuery`]: crate::backend::design::DesignQuery

pub mod dbg;
pub mod frame;
pub mod locate;
pub mod parse;
pub mod session;
pub mod source;
pub mod tcl;

/// Prefix on every message from this module, matching `rwave-wlf` and
/// `rwave-fsdb` elsewhere.
///
/// [`diag::bridge_err`] would be the natural home, but it is gated to the
/// vendor targets and this module deliberately is not.
///
/// [`diag::bridge_err`]: crate::plugin::builtin::diag
pub const ERR_PREFIX: &str = "rwave-questa";

/// Prefix a message for the user. Same shape as `diag::bridge_err`.
pub fn err(msg: impl AsRef<str>) -> String {
    format!("{ERR_PREFIX}: {}", msg.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn err_carries_the_backend_prefix() {
        assert_eq!(err("boom"), "rwave-questa: boom");
    }
}
