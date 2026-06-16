// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Command implementations for `rwave`.
//!
//! One module per subcommand (`info`, `list`, `dump`, `snapshot`, `compare`,
//! `summary`, `search`); each owns its command's JSON shape and text layout.
//! Cross-command helpers (limit/clip math, selection, value formatting, the
//! streaming threshold) live in [`common`]. The dispatch entry points route a
//! parsed [`Args`]: `--json` goes through the `compute_*` functions, text
//! through the `text_*` renderers. Batch mode reuses these very same functions,
//! which is what keeps batch output byte-identical to the equivalent
//! single-command call.
//!
//! Value comparisons use the raw decoded value strings (bit strings for logic,
//! the literal for real/string): they compare pre-format values.

mod common;
mod compare;
mod dump;
mod info;
mod list;
mod search;
mod snapshot;
mod summary;

use crate::cli::{Args, Command};
use crate::json::Json;
use crate::model::Wave;

use common::print_json;
use compare::{compute_compare, text_compare};
use dump::{compute_dump, text_dump};
use info::{compute_info, text_info};
use list::{compute_list, text_list};
use search::{compute_search, text_search};
use snapshot::{compute_snapshot, text_snapshot};
use summary::{compute_summary, text_summary};

/// Dispatch a parsed command (single-command path). `--json` goes through the
/// shared [`compute`] functions; text output uses the per-command `text_*`
/// renderers. Batch mode reuses these very same functions, which is what keeps
/// batch output byte-identical to the equivalent single-command call.
pub fn run(wave: &mut Wave, args: &Args) -> Result<(), String> {
    if args.json {
        let value = compute(wave, args)?;
        print_json(&value);
        return Ok(());
    }
    render_text(wave, args)
}

/// Render a command's text output to stdout, without the `--json` branch. Shared
/// by the single-command text path ([`run`]) and the batch runner's text mode,
/// so a batch text block is identical to the equivalent single command.
pub fn render_text(wave: &mut Wave, args: &Args) -> Result<(), String> {
    match args.command {
        Command::Info => text_info(wave, args),
        Command::List => text_list(wave, args),
        Command::Dump => text_dump(wave, args),
        Command::Summary => text_summary(wave, args),
        Command::Snapshot => text_snapshot(wave, args),
        Command::Compare => text_compare(wave, args),
        Command::Search => text_search(wave, args),
    }
}

/// Compute a command's `--json` result as a [`Json`] value, without printing it.
/// This is the single source of truth for structured output: the single-command
/// `--json` path ([`run`]) and the batch runner both call it, so a batch
/// `result` is byte-for-byte identical to the equivalent `rwave --json …`.
pub fn compute(wave: &mut Wave, args: &Args) -> Result<Json, String> {
    match args.command {
        Command::Info => compute_info(wave, args),
        Command::List => compute_list(wave, args),
        Command::Dump => compute_dump(wave, args),
        Command::Summary => compute_summary(wave, args),
        Command::Snapshot => compute_snapshot(wave, args),
        Command::Compare => compute_compare(wave, args),
        Command::Search => compute_search(wave, args),
    }
}
