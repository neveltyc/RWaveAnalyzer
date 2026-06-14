// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Batch mode: load a waveform file **once**, then read one command per line
//! from stdin and emit one result per command, in input order.
//!
//! This is a pure performance optimization over invoking `rwave` once per
//! query: it collapses N cold file loads into one. It changes nothing about how
//! any single command is computed — each line is parsed by the very same
//! [`crate::cli`] flag parser and rendered by the very same
//! [`crate::commands`] functions as the single-command path, so a batch result
//! is byte-for-byte identical to the equivalent `rwave <cmd> <file> …` call.
//!
//! ## Input
//!
//! Each line is a command minus the leading `rwave` (e.g. `list --filter clk`),
//! tokenized by [`crate::cli::split_line`] (quote-aware) and parsed by
//! [`crate::cli::parse_batch_line`] with the file injected and `[global-opts]`
//! applied as defaults. Blank lines and lines whose first non-blank character
//! is `#` are skipped. A trailing `#label` sets the result's id.
//!
//! ## Output
//!
//! Results stream out in input order, flushed per line. With `--json` each is
//! one NDJSON object `{"id","ok","result"|"error"}`; otherwise each is a
//! `#label` header line followed by that command's normal text output (or a
//! single `Error: …` line on failure).
//!
//! ## Errors
//!
//! A single command failing (bad signal, illegal time, missing required arg, …)
//! is captured as that line's failure and does not stop the batch — exactly the
//! cases that exit non-zero in single-command mode. Only the file failing to
//! load (handled in `main`) or the command stream failing to read is fatal.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use crate::cli::{self, BatchInvocation};
use crate::commands;
use crate::json::{Json, Obj};
use crate::model::Wave;

/// Run the batch loop against an already-loaded `wave`. Returns `SUCCESS` once
/// the whole stream has been processed (even if some commands failed), or
/// `FAILURE` if the command stream itself could not be read.
pub fn run_batch(wave: &mut Wave, inv: &BatchInvocation) -> ExitCode {
    let stdin = io::stdin();
    let handle = stdin.lock();
    let mut seq: u64 = 0;

    for line_res in handle.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                // Invalid UTF-8 or an I/O error on the command stream: fatal. We
                // cannot meaningfully continue reading commands.
                eprintln!("Error: failed to read command stream: {e}");
                let _ = io::stdout().flush();
                return ExitCode::FAILURE;
            }
        };

        let (tokens, label) = match cli::split_line(&line) {
            Ok(parts) => parts,
            Err(msg) => {
                // Malformed line (e.g. an unterminated quote): a failed command.
                seq += 1;
                emit(inv.json, &seq.to_string(), Err(msg));
                let _ = io::stdout().flush();
                continue;
            }
        };

        // Blank line or comment / label-only line: skip without consuming an id.
        if tokens.is_empty() {
            continue;
        }

        seq += 1;
        let id = label.unwrap_or_else(|| seq.to_string());

        match cli::parse_batch_line(&tokens, &inv.file, &inv.defaults) {
            Ok(args) => {
                if inv.json {
                    emit(true, &id, commands::compute(wave, &args));
                } else {
                    // Text mode: header, then the command's normal text output
                    // (identical to the single-command text path), or one
                    // Error line on failure.
                    println!("#{id}");
                    if let Err(msg) = commands::render_text(wave, &args) {
                        println!("Error: {msg}");
                    }
                }
            }
            Err(msg) => emit(inv.json, &id, Err(msg)),
        }
        let _ = io::stdout().flush();
    }

    ExitCode::SUCCESS
}

/// Emit one NDJSON result line (`--json` mode) or, in text mode, a `#id` header
/// plus either nothing (the caller printed the body) or an `Error:` line. This
/// helper only ever handles the JSON framing and the text *error* framing; the
/// text success body is printed by the caller via the command's own renderer.
fn emit(json: bool, id: &str, result: Result<Json, String>) {
    if json {
        let obj = match result {
            Ok(value) => Obj::new()
                .push("id", Json::str(id))
                .push("ok", Json::Bool(true))
                .push("result", value)
                .build(),
            Err(msg) => Obj::new()
                .push("id", Json::str(id))
                .push("ok", Json::Bool(false))
                .push("error", Json::str(msg))
                .build(),
        };
        println!("{}", obj.to_compact_string());
    } else {
        // Text mode reaches here only for the parse-error path (the success
        // body is printed by the caller). Render the header + error line.
        println!("#{id}");
        if let Err(msg) = result {
            println!("Error: {msg}");
        }
    }
}
