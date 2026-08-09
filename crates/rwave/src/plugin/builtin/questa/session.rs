// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! One `vsim -c -view` process, driven over pipes.
//!
//! Lifetime is the rwave process: created on the first `trace`, reused by every
//! later one, and shut down on drop. That is what makes `--batch` cheap — the
//! ~1.5 s startup and the ~340 ms first query are paid once for a whole stream —
//! and it is also why there is no daemon: a live session holds two Questa
//! licences until it exits, and leaving them checked out after the command
//! finished would take them from someone else.
//!
//! Reads are timed out through a channel, because std has no timed pipe read: a
//! thread per stream turns blocking reads into messages the caller can wait on
//! with a deadline.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use super::frame::{marks, wrap, Collector, Response};
use super::{err, ERR_PREFIX};

/// Deadlines. The per-request default is generous because the first query on a
/// large design pays for loading the debug database, and a limit that is too
/// short turns a slow answer into a broken tool.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub per_request: Duration,
    pub handshake: Duration,
    pub teardown: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            per_request: Duration::from_secs(60),
            handshake: Duration::from_secs(30),
            teardown: Duration::from_secs(2),
        }
    }
}

impl Opts {
    /// Honours `RWAVE_VSIM_TIMEOUT_MS` for the per-request deadline.
    pub fn from_env() -> Self {
        let mut o = Self::default();
        if let Some(ms) = std::env::var_os("RWAVE_VSIM_TIMEOUT_MS")
            .and_then(|v| v.to_str().and_then(|s| s.trim().parse::<u64>().ok()))
            .filter(|ms| *ms > 0)
        {
            o.per_request = Duration::from_millis(ms);
        }
        o
    }
}

enum Line {
    Text(String),
    Eof,
}

/// A live `vsim`, or the reason it is no longer usable.
enum State {
    Live,
    /// Once framing is lost there is no safe way back: a later request could be
    /// answered by an earlier one's tail. The session reports the original
    /// reason forever rather than resynchronising.
    Poisoned(String),
}

pub struct VsimSession {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Line>,
    /// Number of streams that have not yet reported EOF.
    open_streams: usize,
    nonce: String,
    seq: u64,
    state: State,
    opts: Opts,
    /// Last lines seen, for the message when the child dies mid-answer.
    tail: Vec<String>,
}

const TAIL: usize = 20;

impl VsimSession {
    /// Spawn `vsim` on `wlf` and complete the readiness handshake.
    ///
    /// The working directory is the waveform's own, which is both Questa's
    /// documented flow for a post-simulation session and what makes the
    /// per-module databases under `<lib>/_dbcontainer` reachable. `-nolog` keeps
    /// vsim from writing a `transcript` file into the user's directory, which it
    /// otherwise does on every run.
    pub fn start(exe: &Path, wlf: &Path, opts: Opts) -> Result<Self, String> {
        let wlf = std::fs::canonicalize(wlf)
            .map_err(|e| err(format!("cannot resolve {}: {e}", wlf.display())))?;
        let dir = wlf
            .parent()
            .ok_or_else(|| err(format!("{} has no parent directory", wlf.display())))?;
        let name = wlf
            .file_name()
            .ok_or_else(|| err(format!("{} has no file name", wlf.display())))?;

        let mut cmd = Command::new(exe);
        cmd.arg("-c")
            .arg("-nolog")
            .arg("-view")
            .arg(name)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::from_command(cmd, opts).map_err(|e| {
            if e.starts_with(ERR_PREFIX) {
                e
            } else {
                err(format!("could not start {}: {e}", exe.display()))
            }
        })
    }

    fn from_command(mut cmd: Command, opts: Opts) -> Result<Self, String> {
        let mut child = cmd
            .spawn()
            .map_err(|e| err(format!("could not start vsim: {e}")))?;
        let stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        let mut open_streams = 0;
        if let Some(out) = child.stdout.take() {
            open_streams += 1;
            let tx = tx.clone();
            thread::spawn(move || pump(out, tx));
        }
        if let Some(errout) = child.stderr.take() {
            open_streams += 1;
            thread::spawn(move || pump(errout, tx));
        }
        let nonce = format!("{}_{}", std::process::id(), open_streams);
        let mut s = Self {
            child,
            stdin,
            rx,
            open_streams,
            nonce,
            seq: 0,
            state: State::Live,
            opts,
            tail: Vec::new(),
        };
        // Prove the protocol before trusting an answer to a real question: if a
        // site init file redefined `echo`, or this build does not read commands
        // from a pipe, that shows up here rather than as a corrupt trace later.
        let probe = s.request_with("echo {rwave-ready}", opts.handshake)?;
        if !probe.data.iter().any(|l| l.contains("rwave-ready")) {
            let why = s.poison("vsim did not answer the readiness probe".to_string());
            return Err(err(format!(
                "{why}. `vsim -c` must read commands from stdin and `echo` must be Questa's \
                 own; check MODELSIM_TCL for an init file that replaces it."
            )));
        }
        Ok(s)
    }

    /// Send one command and collect everything it printed.
    pub fn request(&mut self, cmd: &str) -> Result<Response, String> {
        let t = self.opts.per_request;
        self.request_with(cmd, t)
    }

    fn request_with(&mut self, cmd: &str, timeout: Duration) -> Result<Response, String> {
        if let State::Poisoned(why) = &self.state {
            return Err(why.clone());
        }
        self.seq += 1;
        let m = marks(&self.nonce, self.seq);
        let payload = wrap(cmd, &m);
        let write = self
            .stdin
            .as_mut()
            .ok_or_else(|| "stdin closed".to_string())
            .and_then(|si| {
                si.write_all(payload.as_bytes())
                    .and_then(|()| si.flush())
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = write {
            return Err(self.poison(format!("could not send a command to vsim: {e}")));
        }

        let mut col = Collector::new(m);
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(self.poison(format!(
                    "vsim did not answer `{cmd}` within {}s; the session has been stopped. \
                     Set RWAVE_VSIM_TIMEOUT_MS to allow longer.",
                    timeout.as_secs().max(1)
                )));
            }
            match self.rx.recv_timeout(left) {
                Ok(Line::Text(l)) => {
                    if self.tail.len() == TAIL {
                        self.tail.remove(0);
                    }
                    self.tail.push(l.clone());
                    if col.feed(&l) {
                        return Ok(col.finish());
                    }
                }
                Ok(Line::Eof) => {
                    self.open_streams = self.open_streams.saturating_sub(1);
                    if self.open_streams == 0 {
                        let status = self
                            .child
                            .wait()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|e| format!("unknown status: {e}"));
                        let tail = self.tail.join("\n");
                        return Err(self.poison(format!(
                            "vsim exited ({status}) while answering `{cmd}`.\n{tail}"
                        )));
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.poison(format!("vsim stopped responding to `{cmd}`")));
                }
            }
        }
    }

    /// Record why the session is unusable, stop the child, and return the
    /// message every later call will repeat.
    fn poison(&mut self, why: String) -> String {
        let msg = err(why);
        self.state = State::Poisoned(msg.clone());
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        msg
    }
}

fn pump(stream: impl Read, tx: mpsc::Sender<Line>) {
    let mut r = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                if tx.send(Line::Text(line)).is_err() {
                    return;
                }
            }
        }
    }
    let _ = tx.send(Line::Eof);
}

impl Drop for VsimSession {
    /// Shut vsim down and make sure it is gone.
    ///
    /// This is what returns the licences, so it must not be able to hang: an
    /// unbounded `wait()` on a wedged child would hold rwave open forever, still
    /// holding them. Killing the one pid is enough — `vsim` `execvp`s `vish`,
    /// replacing its own image, so the pid spawned here *is* the process doing
    /// the work; there is no group to signal.
    ///
    /// Dropping stdin is the backstop for the case where this never runs at all
    /// (rwave killed outright): the kernel closes the pipe, vsim reads EOF and
    /// quits by itself. It is also why `main` must keep returning `ExitCode`
    /// rather than calling `process::exit`, which would skip every destructor.
    fn drop(&mut self) {
        if matches!(self.state, State::Live) {
            if let Some(si) = self.stdin.as_mut() {
                let _ = si.write_all(b"quit -f\n");
                let _ = si.flush();
            }
        }
        self.stdin = None;
        let deadline = Instant::now() + self.opts.teardown;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stand-in for vsim: echoes the content of `echo {…}` back with the `# `
    /// prefix Questa uses, so the framing under test is the real one.
    const ECHOING: &str = r#"
while IFS= read -r l; do
  case "$l" in
    quit*) exit 0 ;;
    "echo {"*) m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m" ;;
    *) printf '# %s\n' "$l" ;;
  esac
done
"#;

    fn fake(script: &str, opts: Opts) -> Result<VsimSession, String> {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        VsimSession::from_command(c, opts)
    }

    fn quick() -> Opts {
        Opts {
            per_request: Duration::from_millis(700),
            handshake: Duration::from_millis(700),
            teardown: Duration::from_millis(300),
        }
    }

    #[test]
    fn the_handshake_proves_the_protocol_before_any_real_question() {
        let mut s = fake(ECHOING, quick()).expect("handshake");
        let r = s.request("find drivers -possible -tcl {/top/res}").unwrap();
        assert!(r.begun);
        assert_eq!(r.data, vec!["find drivers -possible -tcl {/top/res}"]);
    }

    #[test]
    fn a_child_that_never_answers_fails_the_handshake() {
        let e = match fake("cat > /dev/null", quick()) {
            Err(e) => e,
            Ok(_) => panic!("a child that never answers must not pass the handshake"),
        };
        assert!(e.contains("did not answer"), "{e}");
    }

    #[test]
    fn a_child_that_frames_correctly_but_answers_nothing_is_still_refused() {
        // The dangerous shape: markers come back, so framing looks healthy, but
        // the command between them did nothing. Caught at the handshake instead
        // of surfacing later as a trace with no drivers.
        let script = r#"
while IFS= read -r l; do
  case "$l" in
    quit*) exit 0 ;;
    "echo {__RWAVE_"*) m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m" ;;
    *) ;;
  esac
done
"#;
        let e = match fake(script, quick()) {
            Err(e) => e,
            Ok(_) => panic!("framing alone must not count as readiness"),
        };
        assert!(e.contains("readiness probe"), "{e}");
        assert!(e.contains("MODELSIM_TCL"), "{e}");
    }

    #[test]
    fn each_request_gets_its_own_markers() {
        let mut s = fake(ECHOING, quick()).unwrap();
        for i in 0..3 {
            let r = s.request(&format!("cmd{i}")).unwrap();
            assert_eq!(r.data, vec![format!("cmd{i}")]);
        }
        assert_eq!(s.seq, 4, "one handshake plus three requests");
    }

    #[test]
    fn a_command_that_prints_nothing_comes_back_provably_silent() {
        // `readers` on an unknown name is silent; the frame still closes.
        let script = r#"
while IFS= read -r l; do
  case "$l" in
    quit*) exit 0 ;;
    "echo {"*) m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m" ;;
    *) ;;
  esac
done
"#;
        let mut s = fake(script, quick()).unwrap();
        let r = s.request("readers {/top/nope}").unwrap();
        assert!(r.is_silent());
    }

    #[test]
    fn a_wedged_child_times_out_poisons_and_is_killed() {
        let mut s = fake(&format!("{ECHOING}\n"), quick()).unwrap();
        // Replace the child's behaviour by starting a mute one instead: the
        // handshake needs a live protocol, the timeout needs silence.
        drop(s);

        let script = format!(
            "{}\nsleep 300\n",
            r#"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
"#
        );
        let mut s = fake(&script, quick()).expect("handshake answers, then it goes quiet");
        let e = s.request("find drivers -possible -tcl {/top/res}").unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
        assert!(e.contains("RWAVE_VSIM_TIMEOUT_MS"), "{e}");

        // Poisoned: the same reason, not a second timeout.
        let again = s.request("anything").unwrap_err();
        assert_eq!(again, e);
        // And the child is already gone, so nothing holds a licence.
        assert!(s.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn a_child_that_exits_mid_answer_reports_that_and_not_a_timeout() {
        let script = r#"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
read -r l; m=${l#echo \{}; m=${m%\}}; printf '# %s\n' "$m"
exit 3
"#;
        let mut s = fake(script, quick()).unwrap();
        let e = s.request("readers {/top/res}").unwrap_err();
        // A child that has already gone surfaces either as EOF on the read or
        // as a broken pipe on the write, depending on which happens first.
        // Both name the death; neither is a timeout, which is the distinction
        // that matters — a timeout would send the reader looking for a slow
        // query instead of a dead process.
        assert!(
            e.contains("vsim exited") || e.contains("could not send a command"),
            "{e}"
        );
        assert!(!e.contains("did not answer"), "{e}");
    }

    #[test]
    fn dropping_the_session_ends_the_process() {
        let mut s = fake(ECHOING, quick()).unwrap();
        s.request("cmd").unwrap();
        let pid = s.child.id();
        drop(s);
        // The pid is reaped by Drop, so it can no longer be signalled.
        let alive = unsafe { libc_kill(pid as i32) };
        assert!(!alive, "vsim still running after drop; a licence would stay out");
    }

    /// `kill(pid, 0)` without pulling in a libc dependency: the crate has none.
    unsafe fn libc_kill(pid: i32) -> bool {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid, 0) == 0 }
    }
}
