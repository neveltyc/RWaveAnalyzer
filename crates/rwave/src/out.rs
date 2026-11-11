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
    /// A write error is kept and reported at [`finish`]: a full disk or a
    /// file-size limit must not end as a short result with exit 0.
    Pipe { w: BufWriter<io::Stdout>, failed: Option<io::Error> },
    Tty { left: usize },
    File(FileSink),
}

struct FileSink {
    w: BufWriter<File>,
    /// `None` when writing straight to a target that is not a regular file
    /// (`/dev/null`, a FIFO), which cannot be replaced by a rename.
    temp: Option<PathBuf>,
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
            Dest::Pipe { w: BufWriter::with_capacity(1 << 16, io::stdout()), failed: None }
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
    let shown = abs.display().to_string();
    let cannot = |e: io::Error| CmdError::env(format!("cannot write --output {shown}: {e}"));
    // An existing target is written where it lives: through a symlink rather
    // than over it, and straight into a device or FIFO, which no rename can
    // replace. So is a link whose chain does not end at the file it opens
    // (`/dev/stdout` with stdout redirected resolves through `/dev/fd`).
    let in_place = |abs: PathBuf| -> Result<(PathBuf, Option<PathBuf>, File), CmdError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&abs)
            .map_err(cannot)?;
        Ok((abs, None, file))
    };
    let (abs, temp, file) = match std::fs::metadata(&abs) {
        Ok(m) if !m.is_file() => in_place(abs)?,
        found => {
            let real = follow_links(&abs).map_err(cannot)?;
            // `/dev/fd/N` and `/proc/self/fd/N` name an open file, not a place
            // a sibling temp file could be renamed over.
            let fd_path = real.starts_with("/dev") || real.starts_with("/proc");
            if found.is_ok_and(|m| fd_path || !same_file(&m, &real)) {
                in_place(abs)?
            } else {
                let mut temp = real.clone().into_os_string();
            temp.push(format!(".{}.part", std::process::id()));
            let temp = PathBuf::from(temp);
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)
                    .map_err(cannot)?;
                (real, Some(temp), file)
            }
        }
    };
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

/// `p` with symlinks followed, a dangling one included (`canonicalize` fails
/// on that, and the rename would then replace the link itself), and its
/// directory normalized. A chain that does not end within 40 hops is a loop.
fn follow_links(p: &std::path::Path) -> io::Result<PathBuf> {
    let mut p = p.to_path_buf();
    for _ in 0..40 {
        match std::fs::read_link(&p) {
            Ok(to) => p = p.parent().map_or(to.clone(), |dir| dir.join(&to)),
            Err(_) => {
                return Ok(match (p.parent().map(std::fs::canonicalize), p.file_name()) {
                    (Some(Ok(dir)), Some(name)) => dir.join(name),
                    _ => p,
                });
            }
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// Is `real` the file `m` describes?
#[cfg(unix)]
fn same_file(m: &std::fs::Metadata, real: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(real).is_ok_and(|r| r.dev() == m.dev() && r.ino() == m.ino())
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::path::Path) -> bool {
    true
}

/// Write formatted text to the result destination (what [`out!`] expands to).
pub fn emit(args: fmt::Arguments) {
    with_dest(|dest| match dest {
        Dest::Pipe { w, failed } => {
            // A closed pipe ends the process through SIGPIPE (restored in
            // main); any other error is reported at `finish`.
            if failed.is_none() {
                if let Err(e) = w.write_fmt(args) {
                    *failed = Some(e);
                }
            }
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
        Dest::Pipe { w, failed } => {
            if failed.is_none() {
                if let Err(e) = w.flush() {
                    *failed = Some(e);
                }
            }
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
        Some(Dest::Pipe { mut w, failed }) => {
            match failed.map_or_else(|| w.flush(), Err) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("cannot write the result to stdout: {e}")),
            }
        }
        Some(Dest::File(mut f)) => {
            let flushed = match f.failed.take() {
                Some(e) => Err(e),
                None => f.flush(),
            };
            drop(f.w);
            let published = match (flushed, &f.temp) {
                (Ok(()), Some(temp)) if ok => std::fs::rename(temp, &f.target),
                (Ok(()), _) => Ok(()),
                (Err(e), _) => Err(e),
            };
            if let (true, Some(temp)) = (!ok || published.is_err(), &f.temp) {
                let _ = std::fs::remove_file(temp);
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
                // A receipt that cannot be written has nowhere to be reported.
                let _ = writeln!(io::stdout(), "{receipt}");
            }
            Ok(())
        }
    }
}

/// Run from the panic hook: push out what was already printed and drop an
/// unfinished `--output` file. Release builds abort on panic, so neither
/// [`finish`] nor any destructor runs. `try_lock`: the panic may have been
/// raised while the sink was held.
pub fn on_panic() {
    let Ok(mut guard) = SINK.try_lock() else {
        return;
    };
    match guard.as_mut() {
        Some(Dest::Pipe { w, .. }) => {
            let _ = w.flush();
        }
        Some(Dest::File(f)) => {
            if let Some(temp) = &f.temp {
                let _ = std::fs::remove_file(temp);
            }
        }
        _ => {}
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
