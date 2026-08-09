// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! [`DesignQuery`] for a `.wlf`, answered by QuestaSim.
//!
//! The waveform itself has no connectivity; Questa keeps it in a `.dbg` written
//! by `vopt -debugdb`, which has no readable format and no API. So this asks
//! `vsim` — see [`super::super::questa`] for why that is the only route, and for
//! the session it is asked over.
//!
//! The session is created on the first query and lives as long as the rwave
//! process, so a `--batch` stream pays the ~1.5 s startup once.

use std::path::{Path, PathBuf};

use crate::backend::design::{Direction, DesignQuery, TraceOutcome};
use crate::plugin::builtin::questa::parse::{self, Fault};
use crate::plugin::builtin::questa::session::{Opts, VsimSession};
use crate::plugin::builtin::questa::source::SourceCache;
use crate::plugin::builtin::questa::{err, locate, tcl};

use super::backend::WlfBackend;

/// What the backend keeps between queries.
#[derive(Default)]
pub struct DesignSession {
    vsim: Option<PathBuf>,
    session: Option<VsimSession>,
    /// Where Questa will look for the debug database. Reported to the user and
    /// used to write the message when it turns out not to be usable; never
    /// checked as a precondition.
    dbg: Option<PathBuf>,
    source: SourceCache,
}

impl WlfBackend {
    /// The waveform's own directory, which is where vsim runs.
    fn wlf_dir(&self) -> PathBuf {
        Path::new(self.path())
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Ask vsim one question and hand back the lines it printed.
    fn ask(&mut self, cmd: &str) -> Result<Vec<String>, String> {
        let s = self
            .design
            .session
            .as_mut()
            .ok_or_else(|| err("no vsim session; ensure_design was not called"))?;
        let r = s.request(cmd)?;
        // Questa reports most refusals as ordinary transcript text and returns
        // success, so the whole response is searched rather than a return code.
        let mut all = r.diagnostics.clone();
        all.extend(r.data.iter().cloned());
        if let Some(f) = parse::classify_fault(&all) {
            // "Signal not found" is a real answer about a real signal — the
            // caller turns it into a NotFound verdict — not a broken session.
            if f != Fault::SignalNotFound {
                let dbg = self.design.dbg.clone().unwrap_or_default();
                let alt = self.wlf_dir().join("vsim.dbg");
                return Err(err(parse::fault_message(
                    &f,
                    self.path(),
                    &dbg.display().to_string(),
                    dbg.exists(),
                    alt.exists(),
                )));
            }
            return Ok(Vec::new());
        }
        Ok(r.data)
    }
}

impl DesignQuery for WlfBackend {
    /// Questa finds the debug database itself, by the same basename as the
    /// waveform, falling back to `vsim.dbg`. That rule is reproduced here only
    /// to name the file in output and in errors — the file is never checked for
    /// existence first, because a pre-check that disagrees with vsim would
    /// reject a setup that actually works.
    fn locate_design(&mut self, cli_kdb: Option<&str>) -> Result<PathBuf, String> {
        if cli_kdb.is_some_and(|s| !s.trim().is_empty()) {
            return Err(err(
                "--kdb names a Verdi design library, which a WLF does not have. Questa \
                 loads the debug database with the same basename as the waveform \
                 (<name>.wlf -> <name>.dbg), so there is nothing to point at.",
            ));
        }
        // Fail before a licence is checked out if the tool is not even here.
        if self.design.vsim.is_none() {
            self.design.vsim = Some(locate::locate_vsim()?);
        }
        let dbg = Path::new(self.path()).with_extension("dbg");
        self.design.dbg = Some(dbg.clone());
        Ok(dbg)
    }

    fn ensure_design(&mut self, _db: &Path, top: Option<&str>) -> Result<(), String> {
        if top.is_some_and(|t| !t.trim().is_empty()) {
            return Err(err(
                "--top selects a top module in a Verdi design library. A WLF session opens \
                 the dataset the waveform already names, so there is nothing to choose.",
            ));
        }
        if self.design.session.is_some() {
            return Ok(());
        }
        let exe = self
            .design
            .vsim
            .clone()
            .ok_or_else(|| err("internal: locate_design must run first"))?;
        let wlf = PathBuf::from(self.path());
        self.design.session = Some(VsimSession::start(&exe, &wlf, Opts::from_env())?);
        Ok(())
    }

    fn trace(
        &mut self,
        signal: &str,
        scope: &str,
        dir: Direction,
        control: bool,
    ) -> Result<TraceOutcome, String> {
        if control {
            return Err(err(
                "--control asks the design database which conditions gate an assignment. \
                 Questa's debug database does not carry that, so the flag cannot be honoured \
                 here rather than being quietly ignored.",
            ));
        }
        let q = parse::to_questa(signal, scope);
        let word = tcl::quote_word(&q)?;

        let hops = match dir {
            // `find drivers -possible` is the only query that reports a source
            // location; plain `drivers` gives `line: -1` after simulation.
            Direction::Driver => {
                let data = self.ask(&format!("echo [find drivers -possible -tcl {word}]"))?;
                let rows = parse::parse_tcl_rows(&data);
                if rows.is_empty() && data.iter().any(|l| !l.trim().is_empty()) {
                    // Output that is not rows is Questa refusing in prose.
                    return Err(err(format!(
                        "vsim answered `find drivers` with something rwave cannot read:\n{}",
                        data.join("\n")
                    )));
                }
                let mut hops = parse::rows_to_hops(&rows);
                self.fill_statements(&mut hops);
                hops
            }
            // Questa has no `find loads` in this release ("not yet available"),
            // so loads come from `readers`, which names the reading process but
            // no location.
            Direction::Load => {
                let data = self.ask(&format!("readers {word}"))?;
                parse::endpoints_to_hops(&parse::parse_endpoints(&data, "Reader"))
            }
        };

        if hops.is_empty() {
            // Nothing came back. Separate "no drivers" from a name vsim never
            // saw, so a path-translation bug cannot masquerade as an answer.
            let probe = self.ask(&format!("echo [find signals {word}]"))?;
            if probe.iter().all(|l| l.trim().is_empty()) {
                return Err(err(format!(
                    "vsim does not resolve '{q}' (rwave path '{signal}'). The waveform \
                     carries that signal, so this is a name-translation mismatch rather \
                     than a missing signal."
                )));
            }
        }
        let status = parse::classify(&hops);
        Ok(TraceOutcome { hops, status })
    }
}

impl WlfBackend {
    /// Replace each driver's placeholder statement with the source line it came
    /// from, when that file can be read. Questa stores a location but no text.
    fn fill_statements(&mut self, hops: &mut [crate::backend::design::Hop]) {
        let dir = self.wlf_dir();
        for h in hops.iter_mut() {
            if let (Some(f), Some(l)) = (h.file.clone(), h.line)
                && let Some(text) = self.design.source.line(&dir, &f, l)
            {
                h.statement = text;
            }
        }
    }
}
