// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! `rwave` binary entry point.

use std::process::ExitCode;

use rwave::cli::{self, ParseOutcome};
use rwave::commands;
use rwave::model::Wave;

fn main() -> ExitCode {
    // On Unix, restore default SIGPIPE so piping into `head` etc. doesn't
    // abort the process with a broken-pipe error mid-write.
    #[cfg(unix)]
    restore_sigpipe();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&argv) {
        ParseOutcome::Print(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        ParseOutcome::Error(msg) => {
            eprintln!("rwave: error: {msg}");
            // argparse exits 2 on usage errors; mirror that.
            ExitCode::from(2)
        }
        ParseOutcome::Run(args) => {
            let mut wave = match Wave::open(&args.file) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match commands::run(&mut wave, &args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(msg) => {
                    eprintln!("Error: {msg}");
                    ExitCode::FAILURE
                }
            }
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
