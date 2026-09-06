// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `rwave` binary entry point.

use std::process::ExitCode;

use rwave::batch;
use rwave::cli::{self, cmd_name, Command, ParseOutcome};
use rwave::commands;
use rwave::json::{Json, Obj};
use rwave::model::Wave;

/// Report a failure on stderr, as JSON when the run asked for JSON.
///
/// A caller that passed `--json` gets JSON on both paths, so it has one parse
/// to write instead of a plain-text special case for failures. It stays on
/// stderr and the exit code is unchanged: stdout carries results, and a shell
/// pipeline should still be able to tell the two apart without parsing
/// anything. `command` is null when the parse failed before naming one.
fn fail(json: bool, command: Option<&Command>, msg: &str) {
    if !json {
        eprintln!("Error: {msg}");
        return;
    }
    let obj = Obj::new()
        .push("command", Json::opt_str(command.map(cmd_name)))
        .push("ok", Json::Bool(false))
        .push("error", Json::str(msg))
        .build();
    eprintln!("{}", obj.to_compact_string());
}

fn main() -> ExitCode {
    // On Unix, restore default SIGPIPE so piping into `head` etc. doesn't
    // abort the process with a broken-pipe error mid-write.
    #[cfg(unix)]
    restore_sigpipe();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    // A usage error can be reported before the parse succeeds, so the framing
    // decision is taken from the raw argv rather than from parsed options.
    let want_json = argv.iter().any(|a| a == "--json");
    match cli::parse(&argv) {
        ParseOutcome::Print(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        ParseOutcome::Error(msg) => {
            if want_json {
                fail(true, None, &msg);
            } else {
                eprintln!("rwave: error: {msg}");
            }
            // Exit 2 on usage errors (the conventional CLI usage-error code).
            ExitCode::from(2)
        }
        ParseOutcome::Run(args) => {
            let mut wave = match Wave::open(&args.file) {
                Ok(w) => w,
                Err(e) => {
                    fail(args.json, Some(&args.command), &e.to_string());
                    return ExitCode::FAILURE;
                }
            };
            match commands::run(&mut wave, &args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    fail(args.json, Some(&args.command), &msg);
                    ExitCode::FAILURE
                }
            }
        }
        ParseOutcome::Batch(inv) => {
            // Load the file once; a load failure is fatal (no command could
            // run). Then stream commands from stdin against the loaded model.
            let mut wave = match Wave::open(&inv.file) {
                Ok(w) => w,
                Err(e) => {
                    fail(inv.json, None, &e.to_string());
                    return ExitCode::FAILURE;
                }
            };
            batch::run_batch(&mut wave, &inv)
        }
    }
}

#[cfg(unix)]
fn restore_sigpipe() {
    // SAFETY: setting SIG_DFL for SIGPIPE is a standard, well-defined call.
    unsafe {
        let _ = libc_signal(SIGPIPE, SIG_DFL);
    }
}

// Avoid pulling in the `libc` crate just for SIGPIPE; declare the minimal FFI
// surface ourselves. These constants are stable across Linux/macOS.
#[cfg(unix)]
const SIGPIPE: i32 = 13;
#[cfg(unix)]
const SIG_DFL: usize = 0;

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "signal"]
    fn libc_signal(signum: i32, handler: usize) -> usize;
}
