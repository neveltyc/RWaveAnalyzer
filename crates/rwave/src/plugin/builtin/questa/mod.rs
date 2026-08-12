// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Reading QuestaSim's post-simulation debug database, to answer connectivity
//! questions about a `.wlf`.
//!
//! A WLF records values over time and carries no connectivity at all. Questa
//! keeps that in a separate database (`.dbg`, written by `vopt -debugdb`),
//! which is SQLite behind a replaced 16-byte header — see [`dbg`].
//!
//! Nothing here touches FFI or a vendor library, so unlike the `wlf` module
//! this carries no target gate and its tests run on every platform.

pub mod dbg;
pub mod source;

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

/// rwave's dotted path, given the scope it was resolved in, as a Questa path.
///
/// The scope is passed in rather than re-derived by splitting `path`, because a
/// Verilog escaped identifier may itself contain a dot and splitting would cut
/// it in the wrong place.
/// The scope is re-spelled by replacing its separators, which is exact for
/// every name rwave can represent: its paths are dot-separated and carry no
/// escaping, so a scope component holding a dot of its own — a Verilog
/// `\esc.name` naming an instance rather than the signal — is already
/// ambiguous before it arrives here. The leaf is the case worth protecting and
/// it is protected, by taking the scope as given rather than splitting.
pub fn to_questa(path: &str, scope: &str) -> String {
    let leaf = match path.strip_prefix(scope) {
        Some(rest) if !scope.is_empty() => rest.strip_prefix('.').unwrap_or(rest),
        _ => path,
    };
    if scope.is_empty() {
        return format!("/{leaf}");
    }
    format!("/{}/{}", scope.replace('.', "/"), leaf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn err_carries_the_backend_prefix() {
        assert_eq!(err("boom"), "rwave-questa: boom");
    }

    #[test]
    fn a_path_takes_questas_spelling() {
        assert_eq!(to_questa("top.u_core.res", "top.u_core"), "/top/u_core/res");
        assert_eq!(to_questa("top", ""), "/top");
        // The scope is trusted over splitting, so a dot inside a leaf survives.
        assert_eq!(to_questa("tb.\\foo.bar", "tb"), "/tb/\\foo.bar");
    }
}
