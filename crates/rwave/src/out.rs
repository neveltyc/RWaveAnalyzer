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
    /// A terminal: stdout, or an `--output` target that is one (`None` =
    /// stdout). Same 1 MiB budget and cut either way, no receipt. A write
    /// error on an `--output` terminal is kept and reported at [`finish`].
    Tty { left: usize, to: Option<(File, String)>, failed: Option<io::Error> },
    File(FileSink),
}

struct FileSink {
    w: BufWriter<File>,
    /// `None` when writing straight to a descriptor path, a device or a FIFO
    /// (`/dev/stdout`, `/dev/null`), which cannot be replaced by a rename.
    temp: Option<PathBuf>,
    target: PathBuf,
    lines: u64,
    bytes: u64,
    failed: Option<io::Error>,
    /// Print the receipt on stdout: not when the target is stdout itself,
    /// where it would land in the result.
    receipt: bool,
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
            Dest::Tty { left: TERMINAL_BUDGET, to: None, failed: None }
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
    if target.ends_with('/') {
        return Err(format!("cannot write --output {target}: it names a directory").into());
    }
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
    // A path naming an open descriptor (`/dev/stdout`, `/dev/fd/3`,
    // `/proc/self/fd/1`) is written through a copy of that descriptor, which
    // keeps its offset and append mode: reopening it on Linux resolves to the
    // file behind it, and a rename there would replace what the shell opened.
    // A device or FIFO is written where it is, since no rename can replace it.
    // Anything else goes to a temp file renamed over the real target (through
    // a symlink, not over it) on success.
    let (abs, temp, file) = if let Some(file) = fd_target(&abs).map_err(cannot)? {
        if is_same_file(&file, input) {
            return Err("--output must not be the waveform file itself".to_string().into());
        }
        (abs, None, file)
    } else {
        match std::fs::metadata(&abs) {
            Ok(m) if !m.is_file() => {
                let file = std::fs::OpenOptions::new().write(true).open(&abs).map_err(cannot)?;
                (abs, None, file)
            }
            _ => {
                let real = follow_links(&abs).map_err(cannot)?;
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
    // A terminal, however it was named (`/dev/stdout`, `/dev/tty`), gets what
    // stdout on a terminal gets: the 1 MiB cut, not an unbounded write.
    if temp.is_none() && file.is_terminal() {
        *SINK.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Dest::Tty { left: TERMINAL_BUDGET, to: Some((file, shown)), failed: None });
        return Ok(());
    }
    let receipt = !is_stdout(&file);
    let sink = FileSink {
        w: BufWriter::with_capacity(1 << 16, file),
        temp,
        target: abs,
        lines: 0,
        bytes: 0,
        failed: None,
        receipt,
    };
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Dest::File(sink));
    Ok(())
}

/// `p` with symlinks followed, a dangling one included (`canonicalize` fails
/// on that, and the rename would then replace the link itself), its
/// directory canonicalized so the receipt names where the file landed. A
/// chain that does not end within 40 hops is a loop.
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

/// A copy of the descriptor `p` names, if it (or a link on the way to the
/// file) is a descriptor path, in any spelling the kernel accepts: the path
/// is matched both as given and with its directory canonicalized, which
/// turns `/proc/self`, `/proc/thread-self` and `/dev/../dev/fd` into
/// `/proc/<pid>[/task/<tid>]` and `/dev/fd`.
#[cfg(unix)]
fn fd_target(p: &std::path::Path) -> io::Result<Option<File>> {
    let mut p = p.to_path_buf();
    for _ in 0..40 {
        let canon = match (p.parent().map(std::fs::canonicalize), p.file_name()) {
            (Some(Ok(dir)), Some(name)) => Some(dir.join(name)),
            _ => None,
        };
        let fd = std::iter::once(&p).chain(canon.as_ref()).find_map(|q| q.to_str().and_then(fd_number));
        if let Some(fd) = fd {
            return dup_fd(fd).map(Some);
        }
        match std::fs::read_link(&p) {
            Ok(to) => p = p.parent().map_or(to.clone(), |dir| dir.join(&to)),
            Err(_) => return Ok(None),
        }
    }
    Ok(None)
}

/// The descriptor a path names: `/dev/std{in,out,err}`, `/dev/fd/N`, and on
/// Linux `/proc/{self,thread-self,<pid>}[/task/<tid>]/fd/N` for this process.
#[cfg(unix)]
fn fd_number(s: &str) -> Option<i32> {
    match s {
        "/dev/stdin" => return Some(0),
        "/dev/stdout" => return Some(1),
        "/dev/stderr" => return Some(2),
        _ => {}
    }
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let n = if let Some(n) = s.strip_prefix("/dev/fd/") {
        n
    } else {
        // procfs exists on Linux only; elsewhere these are ordinary paths.
        if !cfg!(target_os = "linux") {
            return None;
        }
        let rest = s.strip_prefix("/proc/")?;
        let (who, rest) = rest.split_once('/')?;
        // Our pid as procfs numbers it, which differs from getpid() when /proc
        // was mounted in another pid namespace.
        let own = std::fs::read_link("/proc/self").ok()?;
        let own = own.to_str()?;
        if !matches!(who, "self" | "thread-self") && who != own {
            return None;
        }
        let rest = match rest.strip_prefix("task/") {
            // A task path is trusted only in its canonical form, which exists
            // only for a real thread of ours.
            Some(_) if who != own => return None,
            Some(t) => {
                let (tid, r) = t.split_once('/')?;
                if !digits(tid) || !std::path::Path::new(&format!("/proc/{own}/task/{tid}")).exists() {
                    return None;
                }
                r
            }
            None => rest,
        };
        rest.strip_prefix("fd/")?
    };
    // Linux's spelling: digits, no sign, no leading zero (macOS also takes
    // `01`; such a path is then opened as an ordinary device instead).
    if !digits(n) || (n.len() > 1 && n.starts_with('0')) {
        return None;
    }
    n.parse::<i32>().ok()
}

/// Duplicate `fd`, failing with EBADF when it is not open.
#[cfg(unix)]
fn dup_fd(fd: i32) -> io::Result<File> {
    use std::os::fd::BorrowedFd;
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_GETFD: i32 = 1; // the same on Linux and macOS
    // SAFETY: F_GETFD only reads the descriptor flags; it fails for a
    // descriptor that is not open, so `borrow_raw` below gets an open one.
    if unsafe { fcntl(fd, F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is open (checked above) and is only borrowed to be
    // duplicated; the original stays owned by whoever opened it.
    let dup = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    Ok(File::from(dup))
}

/// Is the open `file` the one our stdout writes to?
#[cfg(unix)]
fn is_stdout(file: &File) -> bool {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;
    let out = io::stdout().as_fd().try_clone_to_owned().map(File::from);
    match (file.metadata(), out.and_then(|o| o.metadata())) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_stdout(_: &File) -> bool {
    false
}

/// Is the open `file` the waveform at `input`?
#[cfg(unix)]
fn is_same_file(file: &File, input: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(input)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_same_file(_: &File, _: &str) -> bool {
    false
}

#[cfg(not(unix))]
fn fd_target(_: &std::path::Path) -> io::Result<Option<File>> {
    Ok(None)
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
        Dest::Tty { left, to, failed } => {
            if failed.is_some() {
                return;
            }
            let text = fmt::format(args);
            let named = to.is_some();
            let mut out: Box<dyn Write> = match to {
                Some((f, _)) => Box::new(f),
                None => Box::new(io::stdout().lock()),
            };
            if text.len() <= *left {
                *left -= text.len();
                if let (Err(e), true) = (out.write_all(text.as_bytes()), named) {
                    *failed = Some(e);
                }
                return;
            }
            // Over budget: end on the last whole line that fits. Only output
            // with no line break in the whole budget (one huge JSON line) is
            // shown cut mid-line.
            let head = &text.as_bytes()[..*left];
            let written = match head.iter().rposition(|b| *b == b'\n') {
                Some(nl) => out.write_all(&head[..=nl]),
                None if *left == TERMINAL_BUDGET => {
                    let mut end = *left;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    out.write_all(&text.as_bytes()[..end]).and_then(|()| out.write_all(b"\n"))
                }
                None => Ok(()),
            }
            .and_then(|()| out.flush());
            // A target that cannot be written was never shown: report that
            // (exit 1) rather than a cut.
            if let (Err(e), true) = (written, named) {
                *failed = Some(e);
                return;
            }
            let hint = if named {
                "Send the whole result to a file or a pipe instead of a terminal."
            } else {
                "Write the whole result to a file with --output FILE, or pipe it (| less, | grep, > file)."
            };
            eprintln!("\n>> OUTPUT CUT: {} MiB shown on the terminal. {hint}", TERMINAL_BUDGET >> 20);
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
        None | Some(Dest::Tty { failed: None, .. }) => Ok(()),
        Some(Dest::Tty { failed: Some(e), to, .. }) => {
            let name = to.map(|(_, n)| n).unwrap_or_default();
            Err(format!("cannot write --output {name}: {e}"))
        }
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
            if ok && f.receipt {
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

#[cfg(all(test, unix))]
mod tests {
    use super::fd_number;

    #[test]
    fn descriptor_spellings() {
        for (path, fd) in [("/dev/stdin", 0), ("/dev/stdout", 1), ("/dev/stderr", 2), ("/dev/fd/0", 0), ("/dev/fd/12", 12)] {
            assert_eq!(fd_number(path), Some(fd), "{path}");
        }
        for path in ["/dev/fd/-1", "/dev/fd/+1", "/dev/fd/01", "/dev/fd/", "/dev/fd/1x", "/dev/fd/99999999999", "/dev/stdout2"] {
            assert_eq!(fd_number(path), None, "{path}");
        }
        if cfg!(target_os = "linux") {
            assert_eq!(fd_number("/proc/self/fd/1"), Some(1));
            assert_eq!(fd_number("/proc/thread-self/fd/3"), Some(3));
            assert_eq!(fd_number("/proc/self/task/1/fd/1"), None, "a task path counts only canonical");
            assert_eq!(fd_number("/proc/0/fd/1"), None, "another process");
        } else {
            assert_eq!(fd_number("/proc/self/fd/1"), None);
        }
    }
}
