// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Framing one request/response pair on `vsim`'s stdin/stdout.
//!
//! `vsim -c` reading a pipe echoes each command it takes as `VSIM <n>> <cmd>`
//! and prefixes everything it prints with `# `. A subprocess it shells out to
//! (`vlib`, during a `find drivers`) writes to stderr with no prefix at all, so
//! both streams are collected and neither prefix can be assumed.
//!
//! Two markers per request, not one. Some commands print nothing for a signal
//! that does not exist — `readers` is silent — and a single trailing marker
//! cannot tell "there was no answer" from "the answer has not arrived yet".
//! With a marker on each side, `begun && ended && no payload` *proves* silence.
//! The sequence number in each marker means a stale response from a request
//! that already timed out can never be mistaken for the current one.
//!
//! Markers are written `echo {…}` rather than `echo "…"`. Inside double quotes
//! Tcl performs command substitution, so a marker containing brackets would be
//! executed; braces make the word literal.

/// The pair of markers bracketing one request.
#[derive(Debug, Clone)]
pub struct Marks {
    pub begin: String,
    pub end: String,
}

/// Markers for request `seq` of the session identified by `nonce`.
pub fn marks(nonce: &str, seq: u64) -> Marks {
    Marks {
        begin: format!("__RWAVE_{nonce}_B_{seq}__"),
        end: format!("__RWAVE_{nonce}_E_{seq}__"),
    }
}

/// The exact bytes written to vsim's stdin for one request: marker, command,
/// marker. A Tcl error in `cmd` does not abort the stream — vsim reports it and
/// reads the next line — so the closing marker still arrives.
pub fn wrap(cmd: &str, m: &Marks) -> String {
    format!("echo {{{}}}\n{}\necho {{{}}}\n", m.begin, cmd, m.end)
}

/// True for a line that is vsim echoing back a command it read.
///
/// These are skipped rather than stripped: the echo of `echo {…END…}` contains
/// the end marker, so stripping the prompt and then matching would close the
/// frame one line early, before the payload it was meant to bracket.
pub fn is_command_echo(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("VSIM ") else {
        return false;
    };
    // First `>`, not the last: a command may itself contain one.
    let Some(idx) = rest.find('>') else {
        return false;
    };
    !rest[..idx].is_empty() && rest[..idx].chars().all(|c| c.is_ascii_digit())
}

/// Strip the `# ` that vsim puts on everything it prints. Lines from a shelled
/// -out subprocess have no prefix and pass through unchanged.
pub fn strip_hash(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("# ") {
        return rest;
    }
    if line == "#" { "" } else { line }
}

/// Whether a line is Questa reporting a problem rather than answering.
///
/// Questa's own messages are prefixed (`** Error:`, `Error:`, and lowercase
/// `error:` from a shelled-out `vlib`), but several refusals are bare prose —
/// `invalid command name "…"`, `The "find loads" command is not yet available`.
/// Those are caught structurally instead: a line that does not parse as a row
/// is never treated as data. See [`super::parse`].
pub fn is_diagnostic(body: &str) -> bool {
    let t = body.trim_start();
    let head = t.split(':').next().unwrap_or("");
    t.starts_with("** ")
        || matches!(
            head,
            "Error" | "Warning" | "Fatal" | "Note" | "error" | "warning" | "fatal"
        )
}

/// One framed answer.
#[derive(Debug, Default, Clone)]
pub struct Response {
    /// Payload lines, prompt- and `# `-stripped, diagnostics removed.
    pub data: Vec<String>,
    /// Diagnostic lines, in the order they arrived.
    pub diagnostics: Vec<String>,
    /// Whether the opening marker was seen — without it, an empty `data` says
    /// nothing, and with it, an empty `data` is proof the command printed
    /// nothing.
    pub begun: bool,
}

impl Response {
    /// The command produced no output at all. Only meaningful once `begun`.
    pub fn is_silent(&self) -> bool {
        self.begun && self.data.iter().all(|l| l.trim().is_empty())
    }
}

/// Accumulates lines from both streams until the closing marker arrives.
pub struct Collector {
    marks: Marks,
    resp: Response,
    done: bool,
}

impl Collector {
    pub fn new(marks: Marks) -> Self {
        Self { marks, resp: Response::default(), done: false }
    }

    /// Feed one raw line. Returns true once the frame is closed.
    ///
    /// Everything before the opening marker is discarded, which is what makes
    /// the startup banner, licence chatter and any site `modelsim.tcl`
    /// preamble harmless without touching the child's environment.
    pub fn feed(&mut self, raw: &str) -> bool {
        if self.done || is_command_echo(raw) {
            return self.done;
        }
        let body = strip_hash(raw.trim_end_matches(['\r', '\n']));
        if !self.resp.begun {
            if body.contains(&self.marks.begin) {
                self.resp.begun = true;
            }
            return false;
        }
        if body.contains(&self.marks.end) {
            self.done = true;
            return true;
        }
        if is_diagnostic(body) {
            self.resp.diagnostics.push(body.to_string());
        } else {
            self.resp.data.push(body.to_string());
        }
        false
    }

    pub fn finish(self) -> Response {
        self.resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(lines: &[&str]) -> Response {
        let mut c = Collector::new(marks("t", 1));
        for l in lines {
            if c.feed(l) {
                break;
            }
        }
        c.finish()
    }

    #[test]
    fn a_command_that_prints_nothing_is_provably_silent() {
        // `readers` on a name the debug database does not hold says nothing at
        // all; without the opening marker that is indistinguishable from a
        // response still in flight.
        let r = collect(&["# __RWAVE_t_B_1__", "# __RWAVE_t_E_1__"]);
        assert!(r.begun);
        assert!(r.is_silent());
        assert!(r.data.is_empty());
    }

    #[test]
    fn startup_noise_before_the_opening_marker_is_discarded() {
        let r = collect(&[
            "Reading pref.tcl",
            "# //  Questa Sim-64",
            "# trace.wlf opened as dataset \"trace\"",
            "# __RWAVE_t_B_1__",
            "# {Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4}",
            "# __RWAVE_t_E_1__",
        ]);
        assert_eq!(r.data, vec!["{Gate /top/u_core/u_alu {res_q[7:0]} dut.sv:4}"]);
    }

    #[test]
    fn the_echo_of_the_closing_marker_does_not_close_the_frame_early() {
        // vsim echoes `VSIM 4> echo {…E…}` before printing `# …E…`. Matching
        // the echo would drop any payload that came after it.
        let mut c = Collector::new(marks("t", 1));
        assert!(!c.feed("VSIM 2> echo {__RWAVE_t_B_1__}"));
        assert!(!c.feed("# __RWAVE_t_B_1__"));
        assert!(!c.feed("VSIM 3> echo [find drivers -possible -tcl {/top/res}]"));
        assert!(!c.feed("# {FF /top {res[7:0]} top.sv:9}"));
        assert!(!c.feed("VSIM 4> echo {__RWAVE_t_E_1__}"));
        assert!(c.feed("# __RWAVE_t_E_1__"));
        assert_eq!(c.finish().data, vec!["{FF /top {res[7:0]} top.sv:9}"]);
    }

    #[test]
    fn a_stale_marker_from_an_earlier_request_is_ignored() {
        let mut c = Collector::new(marks("t", 3));
        assert!(!c.feed("# __RWAVE_t_E_2__"));
        assert!(!c.feed("# __RWAVE_t_B_3__"));
        assert!(!c.feed("# payload"));
        assert!(c.feed("# __RWAVE_t_E_3__"));
        assert_eq!(c.finish().data, vec!["payload"]);
    }

    #[test]
    fn diagnostics_are_kept_but_out_of_the_payload() {
        let r = collect(&[
            "# __RWAVE_t_B_1__",
            "# Error: Signal not found (/top/no_such_xyz)",
            "error: \"vlib -libcmd exemptpath work/_dbcontainer/x.dbg\" failed!",
            "#  Warning: onerror command for use within macro",
            "# __RWAVE_t_E_1__",
        ]);
        assert!(r.data.is_empty());
        assert_eq!(r.diagnostics.len(), 3);
        assert!(r.diagnostics[0].contains("Signal not found"));
    }

    #[test]
    fn recognises_the_prompt_only_where_questa_emits_it() {
        assert!(is_command_echo("VSIM 1> quit -f"));
        assert!(is_command_echo("VSIM 36> echo {x}"));
        assert!(!is_command_echo("# VSIM 1> not a prompt"));
        assert!(!is_command_echo("VSIM but no prompt"));
        assert!(!is_command_echo("# {Gate /top <???> dut.sv:4}"));
    }

    #[test]
    fn strips_only_questas_own_prefix() {
        assert_eq!(strip_hash("# hello"), "hello");
        assert_eq!(strip_hash("#"), "");
        assert_eq!(strip_hash("error: raw"), "error: raw");
        // A payload line is never re-stripped: `# # x` is the text `# x`.
        assert_eq!(strip_hash("# # x"), "# x");
    }

    #[test]
    fn wraps_markers_in_braces_so_tcl_cannot_substitute_them() {
        let m = marks("t", 7);
        let w = wrap("readers {/top/res}", &m);
        assert!(w.starts_with("echo {__RWAVE_t_B_7__}\n"));
        assert!(w.ends_with("echo {__RWAVE_t_E_7__}\n"));
        assert!(w.contains("\nreaders {/top/res}\n"));
    }
}
