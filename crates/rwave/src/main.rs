// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `rwave` binary entry point.
//!
//! Exit status says who can fix a failure:
//!
//! | code | meaning |
//! |---|---|
//! | 0 | answered — an empty result too, which says why in `hint` / text |
//! | 1 | the file or environment: cannot open/parse the waveform, design library, licence, `--output` |
//! | 2 | the request: usage, flags, names, conditions, times — change the command |
//! | 3 | terminal output cut at 1 MiB; the result is incomplete |

use std::process::ExitCode;

use rwave::batch;
use rwave::cli::{self, cmd_name, Command, ParseOutcome};
use rwave::commands;
use rwave::json::{Json, Obj};
use rwave::model::Wave;
use rwave::out;

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
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        out::on_panic();
    }));

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
            let cmd = Some(&args.command);
            let name = Some(cmd_name(&args.command));
            if let Some(target) = &args.output {
                if let Err(e) = out::to_file(target, &args.file) {
                    fail(args.json, cmd, &e.msg);
                    return exit_for(&e);
                }
            }
            let mut wave = match Wave::open(&args.file) {
                Ok(w) => w,
                Err(e) => {
                    let _ = out::finish(false, args.json, name);
                    fail(args.json, cmd, &e.to_string());
                    return ExitCode::FAILURE;
                }
            };
            let result = commands::run(&mut wave, &args);
            let published = out::finish(result.is_ok(), args.json, name);
            match (result, published) {
                (Ok(()), Ok(())) => ExitCode::SUCCESS,
                (Ok(()), Err(msg)) => {
                    fail(args.json, cmd, &msg);
                    ExitCode::FAILURE
                }
                (Err(e), _) => {
                    fail(args.json, cmd, &e.msg);
                    exit_for(&e)
                }
            }
        }
        ParseOutcome::Batch(inv) => {
            if let Some(target) = &inv.output {
                if let Err(e) = out::to_file(target, &inv.file) {
                    fail(inv.json, None, &e.msg);
                    return exit_for(&e);
                }
            }
            // Load the file once; a load failure is fatal (no command could
            // run). Then stream commands from stdin against the loaded model.
            let mut wave = match Wave::open(&inv.file) {
                Ok(w) => w,
                Err(e) => {
                    let _ = out::finish(false, inv.json, None);
                    fail(inv.json, None, &e.to_string());
                    return ExitCode::FAILURE;
                }
            };
            let code = batch::run_batch(&mut wave, &inv);
            match out::finish(code == ExitCode::SUCCESS, inv.json, None) {
                Ok(()) => code,
                Err(msg) => {
                    fail(inv.json, None, &msg);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// 1 for the environment's failures, 2 for the request's (see the table above).
fn exit_for(e: &commands::CmdError) -> ExitCode {
    if e.env {
        ExitCode::FAILURE
    } else {
        ExitCode::from(2)
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
