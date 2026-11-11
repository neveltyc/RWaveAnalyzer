// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Where results go. Every result line is written through [`outln!`] /
//! [`out!`], so one place decides the destination:
//!
//! | stdout is | policy |
//! |---|---|
//! | `--output FILE` | whole result to FILE (published atomically on success); stdout gets a one-line receipt |
//! | a pipe or redirect | everything, buffered |
//! | an interactive terminal | stops after [`TERMINAL_BUDGET`] on a line boundary, a notice on stderr, exit [`EXIT_CUT`] |
//!
//! A `--limit 0` dump of a big trace is hundreds of MB that nobody reads on a
//! terminal and that takes minutes to scroll past; a pipe or a file chose where
//! it goes.

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::commands::CmdError;

/// Bytes shown on an interactive terminal before the output is cut.
pub const TERMINAL_BUDGET: usize = 1 << 20;
/// Exit status of a run whose terminal output was cut: the result is
/// incomplete (possibly half a JSON document), which a script driving a
/// pseudo-terminal must be able to tell from a complete one.
pub const EXIT_CUT: i32 = 3;

enum Dest {
    Pipe(BufWriter<io::Stdout>),
    Tty { left: usize },
    File(FileSink),
}

struct FileSink {
    w: BufWriter<File>,
    temp: PathBuf,
    target: PathBuf,
    lines: u64,
    bytes: u64,
    failed: Option<io::Error>,
}

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.w.write(buf)?;
        self.bytes += n as u64;
        self.lines += buf[..n].iter().filter(|b| **b == b'\n').count() as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

static SINK: Mutex<Option<Dest>> = Mutex::new(None);

fn with_dest<R>(f: impl FnOnce(&mut Dest) -> R) -> R {
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let dest = guard.get_or_insert_with(|| {
        if io::stdout().is_terminal() {
            Dest::Tty { left: TERMINAL_BUDGET }
        } else {
            Dest::Pipe(BufWriter::with_capacity(1 << 16, io::stdout()))
        }
    });
    f(dest)
}

/// Send the result to `target` instead of stdout. `input` is the waveform,
/// which `target` must not be. The file appears only at [`finish`]; until then
/// the result is written to `<target>.<pid>.part` next to it. A target that
/// names a directory or the waveform is the request's error; one that cannot
/// be created is the environment's.
pub fn to_file(target: &str, input: &str) -> Result<(), CmdError> {
    let abs = std::path::absolute(target)
        .map_err(|e| CmdError::env(format!("cannot write --output {target}: {e}")))?;
    if abs.is_dir() {
        return Err(format!("cannot write --output {}: it is a directory", abs.display()).into());
    }
    let same = match (std::fs::canonicalize(&abs), std::fs::canonicalize(input)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Err("--output must not be the waveform file itself".to_string().into());
    }
    let mut temp = abs.clone().into_os_string();
    temp.push(format!(".{}.part", std::process::id()));
    let temp = PathBuf::from(temp);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| CmdError::env(format!("cannot write --output {}: {e}", abs.display())))?;
    let sink = FileSink {
        w: BufWriter::with_capacity(1 << 16, file),
        temp,
        target: abs,
        lines: 0,
        bytes: 0,
        failed: None,
    };
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Dest::File(sink));
    Ok(())
}

/// Write formatted text to the result destination (what [`out!`] expands to).
pub fn emit(args: fmt::Arguments) {
    with_dest(|dest| match dest {
        Dest::Pipe(w) => {
            // A closed pipe ends the process through SIGPIPE (restored in
            // main); any other write error has nowhere left to be reported.
            let _ = w.write_fmt(args);
        }
        Dest::File(f) => {
            if f.failed.is_none() {
                if let Err(e) = f.write_fmt(args) {
                    f.failed = Some(e);
                }
            }
        }
        Dest::Tty { left } => {
            let text = fmt::format(args);
            let mut stdout = io::stdout().lock();
            if text.len() <= *left {
                *left -= text.len();
                let _ = stdout.write_all(text.as_bytes());
                return;
            }
            // Over budget: end on the last whole line that fits. Only output
            // with no line break in the whole budget (one huge JSON line) is
            // shown cut mid-line.
            let head = &text.as_bytes()[..*left];
            match head.iter().rposition(|b| *b == b'\n') {
                Some(nl) => {
                    let _ = stdout.write_all(&head[..=nl]);
                }
                None if *left == TERMINAL_BUDGET => {
                    let mut end = *left;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    let _ = stdout.write_all(&text.as_bytes()[..end]);
                    let _ = stdout.write_all(b"\n");
                }
                None => {}
            }
            let _ = stdout.flush();
            eprintln!(
                "\n>> OUTPUT CUT: {} MiB shown on the terminal. Write the whole result to a file \
                 with --output FILE, or pipe it (| less, | grep, > file).",
                TERMINAL_BUDGET >> 20
            );
            std::process::exit(EXIT_CUT);
        }
    })
}

/// Push buffered results out now (batch mode, between lines).
pub fn flush() {
    with_dest(|dest| match dest {
        Dest::Pipe(w) => {
            let _ = w.flush();
        }
        Dest::File(f) => {
            if f.failed.is_none() {
                if let Err(e) = f.flush() {
                    f.failed = Some(e);
                }
            }
        }
        Dest::Tty { .. } => {}
    })
}

/// End the run: flush, and on success publish an `--output` file and print its
/// receipt (`command` names the command for the JSON receipt; `None` in batch
/// mode). On failure an unfinished `--output` file is removed, never published.
pub fn finish(ok: bool, json: bool, command: Option<&str>) -> Result<(), String> {
    let dest = SINK.lock().unwrap_or_else(|e| e.into_inner()).take();
    match dest {
        None | Some(Dest::Tty { .. }) => Ok(()),
        Some(Dest::Pipe(mut w)) => {
            let _ = w.flush();
            Ok(())
        }
        Some(Dest::File(mut f)) => {
            let flushed = match f.failed.take() {
                Some(e) => Err(e),
                None => f.flush(),
            };
            drop(f.w);
            let published = match flushed {
                Ok(()) if ok => std::fs::rename(&f.temp, &f.target),
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            };
            if !ok || published.is_err() {
                let _ = std::fs::remove_file(&f.temp);
            }
            if let Err(e) = published {
                return Err(format!("cannot write --output {}: {e}", f.target.display()));
            }
            if ok {
                let path = f.target.display().to_string();
                let receipt = if json {
                    crate::json::Obj::new()
                        .push("command", crate::json::Json::opt_str(command))
                        .push("output", crate::json::Json::str(path))
                        .push("bytes", crate::json::Json::Int(f.bytes as i64))
                        .push("lines", crate::json::Json::Int(f.lines as i64))
                        .build()
                        .to_compact_string()
                } else {
                    format!("Wrote {} lines ({} bytes) to {path}", f.lines, f.bytes)
                };
                println!("{receipt}");
            }
            Ok(())
        }
    }
}

/// `println!` for results: routed through [`emit`].
#[macro_export]
macro_rules! outln {
    () => {
        $crate::out::emit(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::out::emit(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// `print!` for results: routed through [`emit`].
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::out::emit(format_args!($($arg)*))
    };
}
